//! Client side of the daemon socket, used by terminal commands, the MCP
//! process and the hook. Starts the daemon when it isn't running.

use std::time::Duration;

use imbo_proto::local::{LocalRequest, LocalResponse, WaitReply};
use imbo_proto::{ErrorCode, Problem};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use crate::paths::Home;

pub struct Local {
    pub home: Home,
}

/// How a listener ended (API-LOCAL-010).
pub enum Heard {
    /// Messages, handed to `show` and confirmed.
    Messages,
    /// Another listener already waits for this session.
    Already,
    /// Replaced, withdrawn, or the tool went away: nothing to show.
    Nothing,
}

/// Acting session from the environment (ADR-008): a terminal command run
/// inside an agent session acts as that session. `Err` inside a tool's
/// session that can't be told apart: the command must not act as the
/// person (RULE-005).
pub fn env_actor() -> Result<Option<String>, Problem> {
    crate::adapters::terminal_actor().map_err(|tool| {
        Problem::new(
            ErrorCode::InvalidRequest,
            format!(
                "This command runs inside a {tool} session, but Imbo can't tell which one. Use Imbo's tools in that session instead."
            ),
        )
    })
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
        // Not signed in, as after `imbo uninstall`: the tool processes of
        // sessions still open must not bring the daemon back.
        if self.home.credentials().is_none() {
            return Err(Problem::new(
                ErrorCode::DaemonUnavailable,
                "Imbo isn't set up on this computer. Run `imbo setup` in a terminal.",
            ));
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
        self.exchange(req, wait, Duration::from_secs(15)).await
    }

    /// For uploads and downloads (API-LOCAL-008, 009): the daemon gives up
    /// on a stalled transfer itself, so only a hung daemon hits this limit.
    pub async fn send_slow(&self, req: &LocalRequest) -> Result<LocalResponse, Problem> {
        self.exchange(req, Duration::from_millis(1500), Duration::from_secs(3600))
            .await
    }

    async fn exchange(
        &self,
        req: &LocalRequest,
        wait: Duration,
        reply_within: Duration,
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
        match tokio::time::timeout(reply_within, reader.read_line(&mut buf)).await {
            Ok(Ok(n)) if n > 0 => serde_json::from_str(&buf).map_err(|_| lost()),
            _ => Err(lost()),
        }
    }

    /// API-LOCAL-010: wait for the session's next messages, give them to
    /// `show`, then confirm; `show` returns `false` when it didn't print
    /// them, and nothing is confirmed. Registers again when the daemon
    /// restarts, and ends quietly once the tool process `pid` is gone.
    pub async fn listen(
        &self,
        tool: &str,
        session_id: &str,
        pid: Option<u32>,
        show: impl FnOnce(&str) -> bool,
    ) -> Result<Heard, Problem> {
        let req = LocalRequest::Wait {
            tool: tool.into(),
            session_id: session_id.into(),
            pid,
        };
        let mut line = serde_json::to_vec(&req).unwrap_or_default();
        line.push(b'\n');
        let gone = || pid.is_some_and(|p| !crate::procs::alive(p));
        loop {
            if gone() {
                return Ok(Heard::Nothing);
            }
            let (r, mut w) = self
                .connect_or_start(Duration::from_millis(1500))
                .await?
                .into_split();
            let mut buf = String::new();
            let got = w.write_all(&line).await.is_ok() && {
                let mut reader = BufReader::new(r);
                let read = reader.read_line(&mut buf);
                tokio::pin!(read);
                loop {
                    tokio::select! {
                        n = &mut read => break matches!(n, Ok(n) if n > 0),
                        _ = tokio::time::sleep(Duration::from_secs(2)) => if gone() {
                            return Ok(Heard::Nothing);
                        },
                    }
                }
            };
            if !got {
                // The daemon stopped, as in an upgrade: register again.
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
            let v: Value = serde_json::from_str(&buf).unwrap_or_default();
            if v.get("ok") == Some(&Value::Bool(false)) {
                let problem = serde_json::from_value::<LocalResponse>(v)
                    .ok()
                    .and_then(|r| r.problem);
                return Err(problem.unwrap_or_else(|| {
                    Problem::new(ErrorCode::Unavailable, "Unknown error from the daemon.")
                }));
            }
            let reply: WaitReply = serde_json::from_value(v).unwrap_or_default();
            if reply.already {
                return Ok(Heard::Already);
            }
            let Some(text) = reply.text else {
                return Ok(Heard::Nothing);
            };
            if !show(&text) {
                return Ok(Heard::Nothing);
            }
            let _ = w.write_all(b"printed\n").await;
            return Ok(Heard::Messages);
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
            picture: None,
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

    /// API-LOCAL-002 with a picture file as the body (API-056).
    pub async fn call_picture(
        &self,
        method: &str,
        path: &str,
        picture: String,
        actor: Option<String>,
    ) -> Result<Value, Problem> {
        let req = LocalRequest::Call {
            method: method.into(),
            path: path.into(),
            body: None,
            actor,
            picture: Some(picture),
        };
        self.files_op(&req).await
    }

    /// API-LOCAL-008 / 009 with the problem unwrapped.
    pub async fn files_op(&self, req: &LocalRequest) -> Result<Value, Problem> {
        let res = self.send_slow(req).await?;
        if res.ok {
            Ok(res.body)
        } else {
            Err(res.problem.unwrap_or_else(|| {
                Problem::new(ErrorCode::Unavailable, "Unknown error from the daemon.")
            }))
        }
    }
}
