//! Client side of the daemon socket, used by terminal commands, the MCP
//! process and the hook. Starts the daemon when it isn't running.

use std::time::Duration;

use imbo_proto::local::{LocalRequest, LocalResponse};
use imbo_proto::{ErrorCode, Problem};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use crate::paths::Home;

pub struct Local {
    pub home: Home,
}

/// Acting session from the environment (ADR-008): a terminal command run
/// inside an agent session acts as that session.
pub fn env_actor() -> Option<String> {
    if let Ok(id) = std::env::var("CLAUDE_CODE_SESSION_ID")
        && !id.is_empty() {
            return Some(format!("{}:{id}", imbo_proto::TOOL_CLAUDE));
        }
    if let Ok(id) = std::env::var("CODEX_THREAD_ID")
        && !id.is_empty() {
            return Some(format!("{}:{id}", imbo_proto::TOOL_CODEX));
        }
    None
}

impl Local {
    pub fn new() -> Self {
        Self {
            home: Home::resolve(),
        }
    }

    async fn connect(&self) -> Option<UnixStream> {
        tokio::time::timeout(
            Duration::from_millis(500),
            UnixStream::connect(self.home.socket()),
        )
        .await
        .ok()?
        .ok()
    }

    /// Connect, starting the daemon first if needed (waits up to `wait`).
    pub async fn connect_or_start(&self, wait: Duration) -> Result<UnixStream, Problem> {
        if let Some(s) = self.connect().await {
            return Ok(s);
        }
        crate::service::start_daemon(&self.home);
        let deadline = tokio::time::Instant::now() + wait;
        while tokio::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(100)).await;
            if let Some(s) = self.connect().await {
                return Ok(s);
            }
        }
        Err(Problem::new(
            ErrorCode::DaemonUnavailable,
            "The Imbo background daemon is not running and could not be started. Run `imbo status`.",
        ))
    }

    pub async fn send(&self, req: &LocalRequest) -> Result<LocalResponse, Problem> {
        self.send_with(req, Duration::from_millis(1500)).await
    }

    pub async fn send_with(
        &self,
        req: &LocalRequest,
        wait: Duration,
    ) -> Result<LocalResponse, Problem> {
        let mut stream = self.connect_or_start(wait).await?;
        let mut line = serde_json::to_vec(req).unwrap_or_default();
        line.push(b'\n');
        let lost = || {
            Problem::new(
                ErrorCode::DaemonUnavailable,
                "Lost the connection to the Imbo daemon.",
            )
        };
        stream.write_all(&line).await.map_err(|_| lost())?;
        let mut reader = BufReader::new(stream);
        let mut buf = String::new();
        match tokio::time::timeout(Duration::from_secs(15), reader.read_line(&mut buf)).await {
            Ok(Ok(n)) if n > 0 => serde_json::from_str(&buf).map_err(|_| lost()),
            _ => Err(lost()),
        }
    }

    /// API-LOCAL-002. `Ok` carries the body and the actor's unread count.
    pub async fn call(
        &self,
        method: &str,
        path: &str,
        body: Option<Value>,
        actor: Option<String>,
    ) -> Result<(Value, Option<u32>), Problem> {
        let req = LocalRequest::Call {
            method: method.into(),
            path: path.into(),
            body,
            actor,
        };
        let res = self.send(&req).await?;
        if res.ok {
            Ok((res.body, res.unread))
        } else {
            Err(res.problem.unwrap_or_else(|| {
                Problem::new(ErrorCode::Unavailable, "Unknown error from the daemon.")
            }))
        }
    }
}
