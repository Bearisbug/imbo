//! Talking to `imbo-server`: REST with the problem envelope, and the device
//! WebSocket.

use std::sync::Arc;
use std::time::Duration;

use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper_util::rt::TokioIo;
use imbo_proto::{
    ErrorCode, HEADER_ACTOR, HEADER_FILE_NAME, HEADER_FILE_SHA256, HEADER_REQUEST_ID,
    HEADER_UNREAD, Problem,
};
use serde_json::Value;
use sha2::Digest as _;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::{HeaderValue, StatusCode};

pub const CONNECT_TIMEOUT: Duration = Duration::from_millis(1500);
/// Through a proxy to the Hong Kong server the TLS handshake often takes 1–2 s.
pub const TLS_TIMEOUT: Duration = Duration::from_secs(3);
pub const TOTAL_TIMEOUT: Duration = Duration::from_secs(5);

pub struct ApiResponse {
    pub status: u16,
    pub body: Value,
    pub unread: Option<u32>,
}

pub fn unreachable(server: &str, why: &str) -> Problem {
    Problem::new(
        ErrorCode::ServerUnreachable,
        format!(
            "Cannot reach the Imbo server at {server} ({why}). Check the network, or run `imbo status`."
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
    let body = body.map(|b| (Bytes::from(b.to_string()), "application/json"));
    request_with(server, method, path, token, actor, body, TOTAL_TIMEOUT).await
}

/// Time for a picture of up to 5 MB to go up and be converted (API-056).
pub const PICTURE_TIMEOUT: Duration = Duration::from_secs(60);

/// A REST call whose body is a picture, as `content_type`.
pub async fn request_picture(
    server: &str,
    method: &str,
    path: &str,
    token: &str,
    actor: Option<&str>,
    picture: Vec<u8>,
    content_type: &str,
) -> Result<ApiResponse, Problem> {
    let body = Some((Bytes::from(picture), content_type));
    request_with(
        server,
        method,
        path,
        Some(token),
        actor,
        body,
        PICTURE_TIMEOUT,
    )
    .await
}

async fn request_with(
    server: &str,
    method: &str,
    path: &str,
    token: Option<&str>,
    actor: Option<&str>,
    body: Option<(Bytes, &str)>,
    total: Duration,
) -> Result<ApiResponse, Problem> {
    let method = hyper::Method::from_bytes(method.as_bytes())
        .map_err(|_| Problem::new(ErrorCode::InvalidRequest, "bad method"))?;
    let attempts = if method == hyper::Method::GET { 2 } else { 1 };
    let request_id = uuid::Uuid::now_v7().to_string();
    let mut why = String::new();
    for attempt in 1..=attempts {
        let call = send_once(
            server,
            &method,
            path,
            &request_id,
            token,
            actor,
            body.clone(),
        );
        let got = tokio::time::timeout(total, call)
            .await
            .unwrap_or_else(|_| Err(format!("no answer within {} seconds", total.as_secs())));
        let (status, unread, bytes) = match got {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(%method, path, attempt, why = %e, "server request failed");
                why = e;
                continue;
            }
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
    Err(unreachable(server, &why))
}

async fn send_once(
    server: &str,
    method: &hyper::Method,
    path: &str,
    request_id: &str,
    token: Option<&str>,
    actor: Option<&str>,
    body: Option<(Bytes, &str)>,
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
    let body = match body {
        Some((bytes, kind)) => {
            rb = rb.header("content-type", kind);
            bytes
        }
        None => Bytes::new(),
    };
    let req = rb.body(Full::new(body)).map_err(|e| e.to_string())?;
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

/// A plain-HTTP call to a tool's service on this computer, such as
/// opencode's. Returns the status and the JSON body (`Null` when there is
/// none).
pub async fn local_http(
    url: &str,
    method: &str,
    path: &str,
    auth: Option<&str>,
    body: Option<&Value>,
) -> Result<(u16, Value), String> {
    let call = async {
        let (stream, hostport) = open(url, CONNECT_TIMEOUT).await?;
        let (mut tx, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
            .await
            .map_err(|e| e.to_string())?;
        tokio::spawn(conn);
        let mut rb = hyper::Request::builder()
            .method(method)
            .uri(path)
            .header("host", hostport);
        if let Some(a) = auth {
            rb = rb.header("authorization", a);
        }
        let body = body.map(Value::to_string).unwrap_or_default();
        if !body.is_empty() {
            rb = rb.header("content-type", "application/json");
        }
        let req = rb
            .body(Full::new(Bytes::from(body)))
            .map_err(|e| e.to_string())?;
        let res = tx.send_request(req).await.map_err(|e| e.to_string())?;
        let status = res.status().as_u16();
        let bytes = res
            .into_body()
            .collect()
            .await
            .map_err(|e| e.to_string())?
            .to_bytes();
        Ok((
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        ))
    };
    tokio::time::timeout(TOTAL_TIMEOUT, call)
        .await
        .unwrap_or_else(|_| {
            Err(format!(
                "no answer within {} seconds",
                TOTAL_TIMEOUT.as_secs()
            ))
        })
}

/// A file transfer with no bytes moving for this long is dropped
/// (DESIGN §8 传文件); there is no overall time limit.
pub const STALL: Duration = Duration::from_secs(30);

fn problem_from(status: u16, bytes: &[u8]) -> Problem {
    serde_json::from_slice::<Problem>(bytes).unwrap_or_else(|_| {
        let mut p = Problem::new(
            ErrorCode::Unavailable,
            format!("The server answered HTTP {status}."),
        );
        p.status = status;
        p
    })
}

/// Last time bytes moved, for the stall watchdog.
#[derive(Clone)]
struct Progress(Arc<std::sync::Mutex<tokio::time::Instant>>);

impl Progress {
    fn new() -> Self {
        Self(Arc::new(std::sync::Mutex::new(tokio::time::Instant::now())))
    }
    fn touch(&self) {
        *self.0.lock().unwrap() = tokio::time::Instant::now();
    }
    /// Resolves once nothing has moved for `STALL`.
    async fn stalled(&self) {
        loop {
            let last = *self.0.lock().unwrap();
            if last.elapsed() >= STALL {
                return;
            }
            tokio::time::sleep_until(last + STALL).await;
        }
    }
}

/// API-026: stream `path` (`size` bytes, hash `sha256`) to the server.
pub async fn upload_file(
    server: &str,
    token: &str,
    path: &std::path::Path,
    name: &str,
    size: u64,
    sha256: &str,
) -> Result<ApiResponse, Problem> {
    let file = tokio::fs::File::open(path).await.map_err(|e| {
        Problem::new(
            ErrorCode::FileUnreadable,
            format!("{}: {e}", path.display()),
        )
    })?;
    let progress = Progress::new();
    let p = progress.clone();
    // Sends exactly `size` bytes; a file that shrank meanwhile ends the body
    // early and the server rejects it.
    let chunks = futures::stream::unfold((file, size), move |(mut f, left)| {
        let p = p.clone();
        async move {
            if left == 0 {
                return None;
            }
            let mut buf = vec![0u8; (64 * 1024).min(left as usize)];
            match tokio::io::AsyncReadExt::read(&mut f, &mut buf).await {
                Ok(0) => None,
                Ok(n) => {
                    buf.truncate(n);
                    p.touch();
                    Some((
                        Ok::<_, std::io::Error>(hyper::body::Frame::data(Bytes::from(buf))),
                        (f, left - n as u64),
                    ))
                }
                Err(e) => Some((Err(e), (f, 0))),
            }
        }
    });
    let body = http_body_util::StreamBody::new(Box::pin(chunks));
    let call = async {
        let (stream, hostport) = open(server, CONNECT_TIMEOUT).await?;
        let (mut tx, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
            .await
            .map_err(|e| e.to_string())?;
        tokio::spawn(conn);
        let req = hyper::Request::builder()
            .method(hyper::Method::POST)
            .uri("/v1/files")
            .header("host", hostport)
            .header("user-agent", format!("imbo/{}", crate::VERSION))
            .header(HEADER_REQUEST_ID, uuid::Uuid::now_v7().to_string())
            .header("authorization", format!("Bearer {token}"))
            .header("content-type", "application/octet-stream")
            .header("content-length", size)
            .header(HEADER_FILE_NAME, imbo_proto::text::percent_encode(name))
            .header(HEADER_FILE_SHA256, sha256)
            .body(body)
            .map_err(|e| e.to_string())?;
        let res = tx.send_request(req).await.map_err(|e| e.to_string())?;
        let status = res.status().as_u16();
        let bytes = res
            .into_body()
            .collect()
            .await
            .map_err(|e| e.to_string())?
            .to_bytes();
        Ok::<_, String>((status, bytes))
    };
    let (status, bytes) = tokio::select! {
        r = call => r.map_err(|e| unreachable(server, &e))?,
        _ = progress.stalled() => {
            return Err(unreachable(server, &format!("no data for {} seconds", STALL.as_secs())));
        }
    };
    if (200..300).contains(&status) {
        let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        return Ok(ApiResponse {
            status,
            body,
            unread: None,
        });
    }
    Err(problem_from(status, &bytes))
}

/// API-027: download `/v1/files/{id}` into `dest`. Returns the size and
/// SHA-256 of what was written.
pub async fn download_file(
    server: &str,
    token: &str,
    id: uuid::Uuid,
    dest: &std::path::Path,
) -> Result<(u64, String), Problem> {
    let failed = |e: String| Problem::new(ErrorCode::ServerUnreachable, e);
    let no_answer = || {
        unreachable(
            server,
            &format!("no answer within {} seconds", STALL.as_secs()),
        )
    };
    let (stream, hostport) = open(server, CONNECT_TIMEOUT)
        .await
        .map_err(|e| unreachable(server, &e))?;
    let (mut tx, conn) = hyper::client::conn::http1::handshake(TokioIo::new(stream))
        .await
        .map_err(|e| failed(e.to_string()))?;
    tokio::spawn(conn);
    let req = hyper::Request::builder()
        .method(hyper::Method::GET)
        .uri(format!("/v1/files/{id}"))
        .header("host", hostport)
        .header("user-agent", format!("imbo/{}", crate::VERSION))
        .header(HEADER_REQUEST_ID, uuid::Uuid::now_v7().to_string())
        .header("authorization", format!("Bearer {token}"))
        .body(Full::new(Bytes::new()))
        .map_err(|e| failed(e.to_string()))?;
    let res = tokio::time::timeout(STALL, tx.send_request(req))
        .await
        .map_err(|_| no_answer())?
        .map_err(|e| failed(e.to_string()))?;
    let status = res.status().as_u16();
    let mut body = res.into_body();
    if status != 200 {
        let bytes = tokio::time::timeout(STALL, body.collect())
            .await
            .map_err(|_| no_answer())?
            .map_err(|e| failed(e.to_string()))?
            .to_bytes();
        return Err(problem_from(status, &bytes));
    }
    let mut out = tokio::fs::File::create(dest)
        .await
        .map_err(|e| failed(format!("{}: {e}", dest.display())))?;
    let mut hasher = sha2::Sha256::new();
    let mut size = 0u64;
    loop {
        let frame = tokio::time::timeout(STALL, body.frame())
            .await
            .map_err(|_| failed("no data for 30 seconds".into()))?;
        let Some(frame) = frame else { break };
        let frame = frame.map_err(|e| failed(e.to_string()))?;
        if let Ok(data) = frame.into_data() {
            size += data.len() as u64;
            sha2::Digest::update(&mut hasher, &data);
            tokio::io::AsyncWriteExt::write_all(&mut out, &data)
                .await
                .map_err(|e| failed(format!("{}: {e}", dest.display())))?;
        }
    }
    tokio::io::AsyncWriteExt::flush(&mut out)
        .await
        .map_err(|e| failed(e.to_string()))?;
    Ok((size, hex::encode(sha2::Digest::finalize(hasher))))
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

/// TCP to the server within `connect_timeout`, then TLS for https within
/// `TLS_TIMEOUT`.
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
    let tcp = tokio::time::timeout(connect_timeout, TcpStream::connect((host.as_str(), port)))
        .await
        .map_err(|_| "connect timed out".to_string())?
        .map_err(|e| e.to_string())?;
    tcp.set_nodelay(true).ok();
    let stream: Box<dyn Io> = if tls {
        let name =
            rustls::pki_types::ServerName::try_from(host.clone()).map_err(|e| e.to_string())?;
        let handshake = tokio_rustls::TlsConnector::from(tls_config()).connect(name, tcp);
        let conn = tokio::time::timeout(TLS_TIMEOUT, handshake)
            .await
            .map_err(|_| "TLS handshake timed out".to_string())?
            .map_err(|e| format!("TLS: {e}"))?;
        Box::new(conn)
    } else {
        Box::new(tcp)
    };
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
