//! Talking to `imbo-server`: REST with the problem envelope, and the device
//! WebSocket.

use std::sync::Arc;
use std::time::Duration;

use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper_util::rt::TokioIo;
use imbo_proto::{ErrorCode, HEADER_ACTOR, HEADER_REQUEST_ID, HEADER_UNREAD, Problem};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::{HeaderValue, StatusCode};

pub const CONNECT_TIMEOUT: Duration = Duration::from_millis(1500);
pub const TOTAL_TIMEOUT: Duration = Duration::from_secs(5);

pub struct ApiResponse {
    pub status: u16,
    pub body: Value,
    pub unread: Option<u32>,
}

pub fn unreachable(server: &str) -> Problem {
    Problem::new(
        ErrorCode::ServerUnreachable,
        format!(
            "Cannot reach the Imbo server at {server}. Check the network, or run `imbo status`."
        ),
    )
}

/// REST call: one HTTP/1.1 connection per request, over the same TCP/TLS
/// setup as the WebSocket. Don't switch to reqwest: its rustls path links the
/// macOS Security framework, and with it CoreFoundation, which adds about 4 MB
/// of resident memory to every `imbo mcp` process (DESIGN §15 memory budget).
pub async fn request(
    server: &str,
    method: &str,
    path: &str,
    token: Option<&str>,
    actor: Option<&str>,
    body: Option<&Value>,
) -> Result<ApiResponse, Problem> {
    let method = hyper::Method::from_bytes(method.as_bytes())
        .map_err(|_| Problem::new(ErrorCode::InvalidRequest, "bad method"))?;
    let attempts = if method == hyper::Method::GET { 2 } else { 1 };
    let request_id = uuid::Uuid::now_v7().to_string();
    let body = body.map(|b| b.to_string());
    for _ in 0..attempts {
        let call = send_once(
            server,
            &method,
            path,
            &request_id,
            token,
            actor,
            body.as_deref(),
        );
        let Ok(Ok((status, unread, bytes))) = tokio::time::timeout(TOTAL_TIMEOUT, call).await
        else {
            continue;
        };
        let value: Value = if bytes.is_empty() {
            Value::Null
        } else {
            serde_json::from_slice(&bytes).unwrap_or(Value::Null)
        };
        if (200..300).contains(&status) {
            return Ok(ApiResponse {
                status,
                body: value,
                unread,
            });
        }
        let problem = serde_json::from_value::<Problem>(value).unwrap_or_else(|_| {
            let mut p = Problem::new(
                ErrorCode::Unavailable,
                format!("The server answered HTTP {status}."),
            );
            p.status = status;
            p
        });
        return Err(problem);
    }
    Err(unreachable(server))
}

async fn send_once(
    server: &str,
    method: &hyper::Method,
    path: &str,
    request_id: &str,
    token: Option<&str>,
    actor: Option<&str>,
    body: Option<&str>,
) -> Result<(u16, Option<u32>, Bytes), String> {
    let (stream, hostport) = open(server, CONNECT_TIMEOUT).await?;
    let (mut tx, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .map_err(|e| e.to_string())?;
    tokio::spawn(conn);
    let mut rb = hyper::Request::builder()
        .method(method.clone())
        .uri(path)
        .header("host", hostport)
        .header("user-agent", format!("imbo/{}", crate::VERSION))
        .header(HEADER_REQUEST_ID, request_id);
    if let Some(t) = token {
        rb = rb.header("authorization", format!("Bearer {t}"));
    }
    if let Some(a) = actor {
        rb = rb.header(HEADER_ACTOR, a);
    }
    if body.is_some() {
        rb = rb.header("content-type", "application/json");
    }
    let req = rb
        .body(Full::new(Bytes::from(body.unwrap_or("").to_owned())))
        .map_err(|e| e.to_string())?;
    let res = tx.send_request(req).await.map_err(|e| e.to_string())?;
    let status = res.status().as_u16();
    let unread = res
        .headers()
        .get(HEADER_UNREAD)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok());
    let bytes = res
        .into_body()
        .collect()
        .await
        .map_err(|e| e.to_string())?
        .to_bytes();
    Ok((status, unread, bytes))
}

pub trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

pub type Ws = WebSocketStream<Box<dyn Io>>;

pub enum WsError {
    /// Token rejected: stop reconnecting until the user logs in again.
    Unauthorized,
    Other(String),
}

fn tls_config() -> Arc<rustls::ClientConfig> {
    let roots = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    Arc::new(
        rustls::ClientConfig::builder()
            .with_root_certificates(roots)
            .with_no_client_auth(),
    )
}

/// TCP (and TLS for https) to the server, both within `connect_timeout`.
/// Returns the stream and the `host:port` part of the URL.
async fn open(server: &str, connect_timeout: Duration) -> Result<(Box<dyn Io>, String), String> {
    let (tls, rest) = if let Some(r) = server.strip_prefix("https://") {
        (true, r)
    } else if let Some(r) = server.strip_prefix("http://") {
        (false, r)
    } else {
        return Err(format!("bad server URL {server}"));
    };
    let hostport = rest.split('/').next().unwrap_or(rest);
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) => {
            (h.to_string(), p.parse().unwrap_or(443))
        }
        _ => (hostport.to_string(), if tls { 443 } else { 80 }),
    };
    let connect = async {
        let tcp = TcpStream::connect((host.as_str(), port))
            .await
            .map_err(|e| e.to_string())?;
        tcp.set_nodelay(true).ok();
        let stream: Box<dyn Io> = if tls {
            let name =
                rustls::pki_types::ServerName::try_from(host.clone()).map_err(|e| e.to_string())?;
            let conn = tokio_rustls::TlsConnector::from(tls_config())
                .connect(name, tcp)
                .await
                .map_err(|e| format!("TLS: {e}"))?;
            Box::new(conn)
        } else {
            Box::new(tcp)
        };
        Ok::<_, String>(stream)
    };
    let stream = tokio::time::timeout(connect_timeout, connect)
        .await
        .map_err(|_| "connect timed out".to_string())??;
    Ok((stream, hostport.to_string()))
}

pub async fn ws_connect(server: &str, token: &str) -> Result<Ws, WsError> {
    let (stream, hostport) = open(server, CONNECT_TIMEOUT * 4)
        .await
        .map_err(WsError::Other)?;
    let scheme = if server.starts_with("https://") {
        "wss"
    } else {
        "ws"
    };
    let mut req = format!("{scheme}://{hostport}/v1/ws")
        .into_client_request()
        .map_err(|e| WsError::Other(e.to_string()))?;
    req.headers_mut().insert(
        "authorization",
        HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|e| WsError::Other(e.to_string()))?,
    );
    match tokio::time::timeout(
        TOTAL_TIMEOUT * 2,
        tokio_tungstenite::client_async(req, stream),
    )
    .await
    {
        Err(_) => Err(WsError::Other("handshake timed out".into())),
        Ok(Ok((ws, _))) => Ok(ws),
        Ok(Err(tokio_tungstenite::tungstenite::Error::Http(res)))
            if res.status() == StatusCode::UNAUTHORIZED =>
        {
            Err(WsError::Unauthorized)
        }
        Ok(Err(e)) => Err(WsError::Other(e.to_string())),
    }
}
