//! Claude Code: the session's MCP process registers its messaging socket and
//! token with the daemon; the daemon writes an auth line and a user line.
//! The socket format is not in Claude Code's public docs (risk in DESIGN §28).

use std::path::PathBuf;
use std::time::Duration;

use serde::Deserialize;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;

use crate::paths::claude_dir;

pub async fn inject(socket: &str, token: &str, text: &str) -> Result<(), String> {
    if super::fault("claude") {
        return Err("fault injected".into());
    }
    let path = socket.strip_prefix("uds:").unwrap_or(socket);
    let mut s = tokio::time::timeout(Duration::from_secs(2), UnixStream::connect(path))
        .await
        .map_err(|_| "connect timed out".to_string())?
        .map_err(|e| format!("connect {path}: {e}"))?;
    let auth = serde_json::json!({ "type": "auth", "token": token });
    let user =
        serde_json::json!({ "type": "user", "message": { "role": "user", "content": text } });
    let mut buf = serde_json::to_vec(&auth).unwrap_or_default();
    buf.push(b'\n');
    buf.extend(serde_json::to_vec(&user).unwrap_or_default());
    buf.push(b'\n');
    tokio::time::timeout(Duration::from_secs(2), async {
        s.write_all(&buf).await?;
        s.flush().await?;
        s.shutdown().await
    })
    .await
    .map_err(|_| "write timed out".to_string())?
    .map_err(|e| format!("write: {e}"))?;
    Ok(())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionFile {
    pid: Option<u32>,
    session_id: Option<String>,
    name: Option<String>,
}

fn sessions_dir() -> PathBuf {
    claude_dir().join("sessions")
}

fn read(path: &std::path::Path) -> Option<SessionFile> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// Session title for a running Claude Code process.
pub fn title(pid: u32, session_id: &str) -> Option<String> {
    let f = read(&sessions_dir().join(format!("{pid}.json")))?;
    if f.session_id.as_deref() != Some(session_id) {
        return None;
    }
    f.name.filter(|n| !n.trim().is_empty())
}

/// Process ID of a running session, found from `~/.claude/sessions/*.json`.
pub fn pid_for(session_id: &str) -> Option<u32> {
    let dir = std::fs::read_dir(sessions_dir()).ok()?;
    dir.filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| read(&e.path()))
        .find(|f| f.session_id.as_deref() == Some(session_id))
        .and_then(|f| f.pid)
        .filter(|pid| alive(*pid))
}

pub fn alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let r = unsafe { libc::kill(pid as i32, 0) };
    r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}
