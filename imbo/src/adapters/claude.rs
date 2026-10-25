//! Claude Code: the session's MCP process registers its messaging socket and
//! token with the daemon; the daemon writes an auth line and a user line.
//! The socket format is not in Claude Code's public docs (risk in DESIGN §28).

use std::collections::BTreeMap;
use std::io::{BufRead, Read};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use base64::Engine;
use futures::future::BoxFuture;
use imbo_proto::api::MessageView;
use imbo_proto::local::{LocalFile, LocalRequest};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;
use tracing::{info, warn};

use super::{Adapter, Found, Paste, PastedImage, Registration, env};
use crate::local::Local;
use crate::paths::user_home;
use crate::render;

pub static CLAUDE: Claude = Claude {
    sessions: Mutex::new(BTreeMap::new()),
};

/// Inserts that may fail in a row before a session falls back to the
/// prompt hook (FLOW-DELIVER).
const MAX_INJECT_FAILURES: u32 = 3;

pub struct Claude {
    /// Sessions the daemon knows of, by session ID.
    sessions: Mutex<BTreeMap<String, Session>>,
}

/// What the session's tool process registered: its Claude Code process
/// and the messaging socket to insert through.
struct Session {
    pid: u32,
    socket: Option<String>,
    token: Option<String>,
    failures: u32,
}

/// Liveness and title of a session's Claude Code process.
fn found(session_id: &str, pid: u32) -> Found {
    let online = alive(pid);
    let title = if online {
        title(pid, session_id).unwrap_or_default()
    } else {
        String::new()
    };
    Found {
        session_id: session_id.into(),
        title,
        online,
    }
}

impl Adapter for Claude {
    fn id(&self) -> &'static str {
        imbo_proto::TOOL_CLAUDE
    }

    fn name(&self) -> &'static str {
        "Claude Code"
    }

    /// Claude Code gives its MCP processes and shell commands the session
    /// in the environment.
    fn session(&self, _meta: &[&Map<String, Value>]) -> Option<String> {
        env("CLAUDE_CODE_SESSION_ID")
    }

    fn owns_mcp(&self, _client: &str) -> bool {
        self.session(&[]).is_some()
    }

    fn env_pass(&self) -> &'static [&'static str] {
        &["CLAUDE_CONFIG_DIR"]
    }

    fn env_session(&self) -> &'static [&'static str] {
        &[
            "CLAUDE_CODE_SESSION_ID",
            "CLAUDE_CODE_MESSAGING_SOCKET",
            "CLAUDE_CODE_MESSAGING_TOKEN",
        ]
    }

    fn announce(&self) -> Option<Registration> {
        let session_id = self.session(&[])?;
        let pid = pid_for(&session_id).unwrap_or_else(|| unsafe { libc::getppid() } as u32);
        Some(Registration {
            session_id,
            messaging_socket: env("CLAUDE_CODE_MESSAGING_SOCKET"),
            messaging_token: env("CLAUDE_CODE_MESSAGING_TOKEN"),
            pid,
        })
    }

    /// Failures in a row are kept while the same process re-registers the
    /// same socket.
    fn register(&self, r: Registration) -> Option<Found> {
        let mut map = self.sessions.lock().unwrap();
        let failures = match map.get(&r.session_id) {
            Some(s)
                if s.pid == r.pid
                    && s.socket == r.messaging_socket
                    && s.token == r.messaging_token =>
            {
                s.failures
            }
            _ => 0,
        };
        let session = Session {
            pid: r.pid,
            socket: r.messaging_socket,
            token: r.messaging_token,
            failures,
        };
        map.insert(r.session_id.clone(), session);
        Some(found(&r.session_id, r.pid))
    }

    /// A terminal command inside a session that hasn't registered: found
    /// through `~/.claude/sessions`, with no socket to insert through.
    fn adopt(&self, session_id: &str) -> Option<Found> {
        let mut map = self.sessions.lock().unwrap();
        let pid = match map.get(session_id) {
            Some(s) => s.pid,
            None => {
                let pid = pid_for(session_id)?;
                let session = Session {
                    pid,
                    socket: None,
                    token: None,
                    failures: 0,
                };
                map.insert(session_id.into(), session);
                pid
            }
        };
        Some(found(session_id, pid))
    }

    fn poll_every(&self) -> Duration {
        Duration::from_millis(2500)
    }

    /// Re-check the registered processes and titles; closed ones are
    /// forgotten.
    fn discover(&self) -> BoxFuture<'_, Result<Vec<Found>, String>> {
        let mut map = self.sessions.lock().unwrap();
        map.retain(|_, s| alive(s.pid));
        let list = map.iter().map(|(id, s)| found(id, s.pid)).collect();
        Box::pin(std::future::ready(Ok(list)))
    }

    fn channel(&self, session_id: &str) -> &'static str {
        match self.sessions.lock().unwrap().get(session_id) {
            Some(Session {
                socket: Some(_),
                token: Some(_),
                failures,
                ..
            }) if *failures < MAX_INJECT_FAILURES => "inject",
            _ => "prompt_hook",
        }
    }

    /// Without a working socket the session gets its messages through the
    /// prompt hook (API-LOCAL-003).
    fn ready(&self, session_id: &str) -> Result<(), &'static str> {
        match self.channel(session_id) {
            "inject" => Ok(()),
            _ => Err("prompt-hook-fallback"),
        }
    }

    /// Tried 3 times; after that the session falls back to the prompt hook.
    fn deliver<'a>(
        &'a self,
        session_id: &'a str,
        m: &'a MessageView,
        files: &'a [LocalFile],
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let channel = {
                let map = self.sessions.lock().unwrap();
                map.get(session_id)
                    .and_then(|s| Some((s.socket.clone()?, s.token.clone()?)))
            };
            let Some((sock, tok)) = channel else {
                return Err("no messaging socket".into());
            };
            let text = render::for_claude(m, files);
            let mut last_err = String::new();
            let mut done = false;
            for attempt in 0..MAX_INJECT_FAILURES {
                match inject(&sock, &tok, &text).await {
                    Ok(()) => {
                        done = true;
                        break;
                    }
                    Err(e) => {
                        warn!(msg = m.id, session = %session_id, attempt = attempt + 1, error = %e, "claude insert failed");
                        last_err = e;
                        tokio::time::sleep(Duration::from_millis(300)).await;
                    }
                }
            }
            if let Some(s) = self.sessions.lock().unwrap().get_mut(session_id) {
                s.failures = if done { 0 } else { MAX_INJECT_FAILURES };
            }
            if done {
                info!(msg = m.id, session = %session_id, "inserted into Claude Code session");
                Ok(())
            } else {
                warn!(session = %session_id, "insert failed 3 times; this session now gets messages through the prompt hook");
                Err(last_err)
            }
        })
    }

    fn paste(&self) -> Paste {
        Paste::Numbered("用户在会话里粘贴的图片（[Image #N]）把编号写进 images，作为附件发出。")
    }

    fn pasted_image(&self, session_id: &str, n: u32) -> Result<PastedImage, String> {
        transcript_image(session_id, n).ok_or_else(|| {
            format!("会话记录里找不到粘贴的图片 [Image #{n}]，请让使用者重新粘贴后再发。")
        })
    }
}

/// Directory holding the Claude Code state we read (`~/.claude`).
pub fn config_dir() -> PathBuf {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| user_home().join(".claude"))
}

async fn inject(socket: &str, token: &str, text: &str) -> Result<(), String> {
    if super::fault(imbo_proto::TOOL_CLAUDE) {
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
    config_dir().join("sessions")
}

fn read(path: &std::path::Path) -> Option<SessionFile> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// Session title for a running Claude Code process.
fn title(pid: u32, session_id: &str) -> Option<String> {
    let f = read(&sessions_dir().join(format!("{pid}.json")))?;
    if f.session_id.as_deref() != Some(session_id) {
        return None;
    }
    f.name.filter(|n| !n.trim().is_empty())
}

/// Process ID of a running session, found from `~/.claude/sessions/*.json`.
fn pid_for(session_id: &str) -> Option<u32> {
    let dir = std::fs::read_dir(sessions_dir()).ok()?;
    dir.filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| read(&e.path()))
        .find(|f| f.session_id.as_deref() == Some(session_id))
        .and_then(|f| f.pid)
        .filter(|pid| alive(*pid))
}

fn alive(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    let r = unsafe { libc::kill(pid as i32, 0) };
    r == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

/// Image `n` pasted into the session, read from its transcript
/// `~/.claude/projects/<dir>/<session>.jsonl`. The transcript format is not
/// in Claude Code's public docs (risk in DESIGN §28).
fn transcript_image(session_id: &str, n: u32) -> Option<PastedImage> {
    let name = format!("{session_id}.jsonl");
    let path = std::fs::read_dir(config_dir().join("projects"))
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path().join(&name))
        .find(|p| p.is_file())?;
    latest_image(&path, n)
}

/// Numbers get reused within a session, so the last prompt carrying `n` wins.
fn latest_image(transcript: &Path, n: u32) -> Option<PastedImage> {
    let file = std::fs::File::open(transcript).ok()?;
    let mut found = None;
    for line in std::io::BufReader::new(file).lines() {
        let Ok(line) = line else { break };
        if !line.contains("imagePasteIds") {
            continue;
        }
        if let Some(img) = serde_json::from_str::<Value>(&line)
            .ok()
            .and_then(|v| image_in(&v, n))
        {
            found = Some(img);
        }
    }
    found
}

/// A prompt keeps its pasted images in `message.content`, a prompt queued
/// mid-turn in `attachment.prompt`; `imagePasteIds` numbers the image blocks
/// in order.
fn image_in(v: &Value, n: u32) -> Option<PastedImage> {
    let (ids, blocks) = match v["type"].as_str()? {
        "user" => (&v["imagePasteIds"], &v["message"]["content"]),
        "attachment" => (
            &v["attachment"]["imagePasteIds"],
            &v["attachment"]["prompt"],
        ),
        _ => return None,
    };
    let pos = ids
        .as_array()?
        .iter()
        .position(|i| i.as_u64() == Some(u64::from(n)))?;
    let source = &blocks
        .as_array()?
        .iter()
        .filter(|b| b["type"] == "image")
        .nth(pos)?["source"];
    if source["type"] != "base64" {
        return None;
    }
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(source["data"].as_str()?)
        .ok()?;
    Some(PastedImage {
        media_type: source["media_type"].as_str().unwrap_or("image/png").into(),
        bytes,
    })
}

/// `imbo hook prompt`: Claude Code's UserPromptSubmit hook (API-LOCAL-003).
/// Hands unread messages to the session as additional context when live
/// insertion isn't working. Never blocks the prompt: any failure exits 0
/// with no output.
pub async fn prompt_hook(event: String) -> anyhow::Result<()> {
    if event != "prompt" {
        return Ok(());
    }
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    let session_id = serde_json::from_str::<Value>(&input)
        .ok()
        .and_then(|v| v["session_id"].as_str().map(str::to_string))
        .or_else(|| std::env::var("CLAUDE_CODE_SESSION_ID").ok())
        .filter(|s| !s.is_empty());
    let Some(session_id) = session_id else {
        return Ok(());
    };
    let req = LocalRequest::HookPrompt {
        tool: imbo_proto::TOOL_CLAUDE.into(),
        session_id,
    };
    let local = Local::new();
    let Ok(Ok(res)) = tokio::time::timeout(
        Duration::from_secs(8),
        local.send_with(&req, Duration::from_secs(1)),
    )
    .await
    else {
        return Ok(());
    };
    if let Some(ctx) = res
        .body
        .get("context")
        .and_then(Value::as_str)
        .filter(|_| res.ok)
    {
        let out = json!({
            "hookSpecificOutput": { "hookEventName": "UserPromptSubmit", "additionalContext": ctx }
        });
        println!("{out}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn b64(b: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(b)
    }

    fn image(data: &[u8], media_type: &str) -> Value {
        json!({ "type": "image", "source": { "type": "base64", "media_type": media_type, "data": b64(data) } })
    }

    #[test]
    fn finds_images_in_prompts_and_queued_messages_latest_first() {
        let lines = [
            json!({ "type": "user", "imagePasteIds": [1, 2], "message": { "role": "user", "content": [
                { "type": "text", "text": "[Image #1] [Image #2]" },
                image(b"first-1", "image/png"), image(b"first-2", "image/jpeg") ] } }),
            json!({ "type": "assistant", "message": { "content": [] } }),
            json!({ "type": "attachment", "attachment": { "type": "queued_command", "imagePasteIds": [3],
                "prompt": [ { "type": "text", "text": "[Image #3]" }, image(b"queued-3", "image/png") ] } }),
            json!({ "type": "user", "imagePasteIds": [1], "message": { "role": "user", "content": [
                { "type": "text", "text": "[Image #1]" }, image(b"second-1", "image/png") ] } }),
        ];
        let dir = std::env::temp_dir().join(format!("imbo-claude-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("s.jsonl");
        let text: Vec<String> = lines.iter().map(|l| l.to_string()).collect();
        std::fs::write(&path, text.join("\n")).unwrap();

        let one = latest_image(&path, 1).unwrap();
        assert_eq!(one.bytes, b"second-1");
        let two = latest_image(&path, 2).unwrap();
        assert_eq!((two.bytes.as_slice(), two.ext()), (&b"first-2"[..], "jpg"));
        assert_eq!(latest_image(&path, 3).unwrap().bytes, b"queued-3");
        assert!(latest_image(&path, 9).is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
