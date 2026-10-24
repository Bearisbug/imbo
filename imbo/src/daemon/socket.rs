//! `~/.imbo/daemon.sock`: one JSON request per line, one response per line
//! (API-LOCAL-001..003).

use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use imbo_proto::api::PullResponse;
use imbo_proto::local::{DaemonStatus, LocalRequest, LocalResponse};
use imbo_proto::{ErrorCode, Problem, TOOL_CLAUDE};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tracing::{info, warn};

use super::D;
use crate::net;
use crate::paths::Home;

pub fn bind(home: &Home) -> anyhow::Result<UnixListener> {
    let path = home.socket();
    let _ = std::fs::remove_file(&path);
    let l = UnixListener::bind(&path)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    Ok(l)
}

pub async fn serve(d: D, listener: UnixListener) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let d = d.clone();
                tokio::spawn(async move { conn(d, stream).await });
            }
            Err(e) => {
                warn!(error = %e, "accept failed");
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    }
}

async fn conn(d: D, stream: UnixStream) {
    let (r, mut w) = stream.into_split();
    let mut lines = BufReader::new(r).lines();
    while let Ok(Some(line)) = lines.next_line().await {
        let res = match serde_json::from_str::<LocalRequest>(&line) {
            Ok(LocalRequest::Shutdown) => {
                let _ = w
                    .write_all(b"{\"ok\":true,\"status\":200,\"body\":null}\n")
                    .await;
                d.shutdown.notify_one();
                return;
            }
            Ok(req) => handle(&d, req).await,
            Err(e) => LocalResponse::err(Problem::new(
                ErrorCode::InvalidRequest,
                format!("bad request: {e}"),
            )),
        };
        let mut out = serde_json::to_vec(&res).unwrap_or_default();
        out.push(b'\n');
        if w.write_all(&out).await.is_err() {
            return;
        }
    }
}

async fn handle(d: &D, req: LocalRequest) -> LocalResponse {
    match req {
        LocalRequest::Register {
            tool,
            session_id,
            messaging_socket,
            messaging_token,
            pid,
        } => {
            {
                let mut s = d.sessions.lock().unwrap();
                if tool == TOOL_CLAUDE {
                    s.register_claude(&session_id, pid, messaging_socket, messaging_token);
                } else {
                    s.adopt(&tool, &session_id);
                }
            }
            d.flush_presence();
            LocalResponse::ok(200, Value::Null, None)
        }
        LocalRequest::Call {
            method,
            path,
            body,
            actor,
        } => call(d, &method, &path, body, actor).await,
        LocalRequest::HookPrompt { tool, session_id } => hook_prompt(d, &tool, &session_id).await,
        LocalRequest::FakeSession {
            tool,
            session_id,
            title,
            remove,
        } => {
            d.sessions
                .lock()
                .unwrap()
                .set_fake(&tool, &session_id, &title, remove);
            d.flush_presence();
            LocalResponse::ok(200, Value::Null, None)
        }
        LocalRequest::Status => {
            let creds = d.creds();
            let link = d.link.lock().unwrap();
            let status = DaemonStatus {
                version: crate::VERSION.into(),
                pid: std::process::id(),
                server: creds
                    .as_ref()
                    .map(|c| c.server.clone())
                    .unwrap_or_else(|| d.home.config().server),
                logged_in: creds.is_some() && !link.revoked,
                connected: link.connected,
                last_error: link.last_error.clone(),
                sessions: d.sessions.lock().unwrap().list(),
                notices: d.notices.lock().unwrap().iter().cloned().collect(),
            };
            LocalResponse::ok(200, serde_json::to_value(status).unwrap_or_default(), None)
        }
        LocalRequest::Reload => {
            d.reload_creds();
            LocalResponse::ok(200, Value::Null, None)
        }
        LocalRequest::Shutdown => unreachable!("handled in conn"),
    }
}

/// Make sure the acting session is known and online before the server is
/// asked to act as it.
fn ensure_session(d: &D, actor: &str) {
    let Some((tool, sid)) = actor.split_once(':') else {
        return;
    };
    let changed = {
        let mut s = d.sessions.lock().unwrap();
        let before = s
            .get(&(tool.to_string(), sid.to_string()))
            .map(|e| e.online);
        let now = s.adopt(tool, sid);
        before != Some(now)
    };
    if changed {
        d.flush_presence();
    }
}

async fn call(
    d: &D,
    method: &str,
    path: &str,
    body: Option<Value>,
    actor: Option<String>,
) -> LocalResponse {
    let Some(creds) = d.creds() else {
        return LocalResponse::err(Problem::new(
            ErrorCode::NotLoggedIn,
            "This computer is not signed in to Imbo. Run `imbo login` in a terminal.",
        ));
    };
    if let Some(a) = &actor {
        ensure_session(d, a);
    }
    let client = d.http(&creds.server);
    let mut attempt = 0;
    loop {
        let r = net::request(
            &client,
            &creds.server,
            method,
            path,
            Some(&creds.token),
            actor.as_deref(),
            body.as_ref(),
        )
        .await;
        match r {
            Ok(r) => return LocalResponse::ok(r.status, r.body, r.unread),
            // The server hasn't seen this session's presence yet.
            Err(p) if p.is(ErrorCode::UnknownSession) && attempt < 3 => {
                attempt += 1;
                d.flush_presence();
                tokio::time::sleep(Duration::from_millis(400)).await;
            }
            Err(p) if p.is(ErrorCode::Unauthorized) => {
                let mut l = d.link.lock().unwrap();
                l.revoked = true;
                l.last_error = Some("this computer's sign-in was revoked; run `imbo login`".into());
                return LocalResponse::err(Problem::new(
                    ErrorCode::NotLoggedIn,
                    "This computer's Imbo sign-in is no longer valid. Run `imbo login` in a terminal.",
                ));
            }
            Err(p) => return LocalResponse::err(p),
        }
    }
}

/// API-LOCAL-003. Only sessions without a working insert channel pull here,
/// so a message is never both inserted and handed to the hook.
async fn hook_prompt(d: &D, tool: &str, session_id: &str) -> LocalResponse {
    let key = (tool.to_string(), session_id.to_string());
    let channel = {
        let mut s = d.sessions.lock().unwrap();
        if s.get(&key).is_none() {
            s.adopt(tool, session_id);
        }
        s.get(&key).map(|e| e.channel())
    };
    d.flush_presence();
    if channel == Some("inject") {
        return LocalResponse::ok(200, json!({ "context": null }), None);
    }
    let actor = format!("{tool}:{session_id}");
    let r = call(
        d,
        "POST",
        "/v1/inbox/pull?via=prompt_hook",
        None,
        Some(actor),
    )
    .await;
    if !r.ok {
        return r;
    }
    let messages = serde_json::from_value::<PullResponse>(r.body)
        .map(|p| p.messages)
        .unwrap_or_default();
    if messages.is_empty() {
        return LocalResponse::ok(200, json!({ "context": null }), None);
    }
    info!(
        session = session_id,
        count = messages.len(),
        "handed unread messages to the prompt hook"
    );
    LocalResponse::ok(
        200,
        json!({ "context": crate::render::for_hook(&messages) }),
        None,
    )
}
