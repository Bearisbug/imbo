//! Talking to `imbo-server`: REST with the problem envelope, and the device
//! WebSocket.

use std::sync::Arc;
use std::time::Duration;

use imbo_proto::{ErrorCode, HEADER_ACTOR, HEADER_REQUEST_ID, HEADER_UNREAD, Problem};
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::{HeaderValue, StatusCode};

pub const CONNECT_TIMEOUT: Duration = Duration::from_millis(1500);
pub const TOTAL_TIMEOUT: Duration = Duration::from_secs(5);

fn is_loopback(server: &str) -> bool {
    let host = server
        .split("://")
        .nth(1)
        .unwrap_or(server)
        .split(['/', ':'])
        .next()
        .unwrap_or("");
    matches!(host, "127.0.0.1" | "localhost" | "[::1]" | "::1")
}

pub fn http_client(server: &str) -> reqwest::Client {
    let mut b = reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .timeout(TOTAL_TIMEOUT)
        .user_agent(format!("imbo/{}", crate::VERSION));
    if is_loopback(server) {
        b = b.no_proxy();
    }
    b.build().expect("http client")
}

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

pub async fn request(
    client: &reqwest::Client,
    server: &str,
    method: &str,
    path: &str,
    token: Option<&str>,
    actor: Option<&str>,
    body: Option<&Value>,
) -> Result<ApiResponse, Problem> {
    let method = reqwest::Method::from_bytes(method.as_bytes())
        .map_err(|_| Problem::new(ErrorCode::InvalidRequest, "bad method"))?;
    let url = format!("{}{}", server.trim_end_matches('/'), path);
    let attempts = if method == reqwest::Method::GET { 2 } else { 1 };
    let request_id = uuid::Uuid::now_v7().to_string();
    let mut last = None;
    for _ in 0..attempts {
        let mut rb = client
            .request(method.clone(), &url)
            .header(HEADER_REQUEST_ID, &request_id);
        if let Some(t) = token {
            rb = rb.bearer_auth(t);
        }
        if let Some(a) = actor {
            rb = rb.header(HEADER_ACTOR, a);
        }
        if let Some(b) = body {
            rb = rb.json(b);
        }
        match rb.send().await {
            Ok(res) => {
                let status = res.status().as_u16();
                let unread = res
                    .headers()
                    .get(HEADER_UNREAD)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse().ok());
                let bytes = res.bytes().await.map_err(|_| unreachable(server))?;
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
            Err(_) => last = Some(unreachable(server)),
        }
    }
    Err(last.unwrap_or_else(|| unreachable(server)))
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

pub async fn ws_connect(server: &str, token: &str) -> Result<Ws, WsError> {
    let (tls, rest) = if let Some(r) = server.strip_prefix("https://") {
        (true, r)
    } else if let Some(r) = server.strip_prefix("http://") {
        (false, r)
    } else {
        return Err(WsError::Other(format!("bad server URL {server}")));
    };
    let hostport = rest.split('/').next().unwrap_or(rest);
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) if p.chars().all(|c| c.is_ascii_digit()) => {
            (h.to_string(), p.parse().unwrap_or(443))
        }
        _ => (hostport.to_string(), if tls { 443 } else { 80 }),
    };
    let url = format!("{}://{}/v1/ws", if tls { "wss" } else { "ws" }, hostport);
    let mut req = url
        .into_client_request()
        .map_err(|e| WsError::Other(e.to_string()))?;
    req.headers_mut().insert(
        "authorization",
        HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|e| WsError::Other(e.to_string()))?,
    );
    let tcp = tokio::time::timeout(
        CONNECT_TIMEOUT * 4,
        TcpStream::connect((host.as_str(), port)),
    )
    .await
    .map_err(|_| WsError::Other("connect timed out".into()))?
    .map_err(|e| WsError::Other(e.to_string()))?;
    tcp.set_nodelay(true).ok();
    let stream: Box<dyn Io> = if tls {
        let name = rustls::pki_types::ServerName::try_from(host.clone())
            .map_err(|e| WsError::Other(e.to_string()))?;
        let conn = tokio_rustls::TlsConnector::from(tls_config())
            .connect(name, tcp)
            .await
            .map_err(|e| WsError::Other(format!("TLS: {e}")))?;
        Box::new(conn)
    } else {
        Box::new(tcp)
    };
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
