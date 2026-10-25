//! Claude Code: the session's MCP process registers its messaging socket and
//! token with the daemon; the daemon writes an auth line and a user line.
//! The socket format is not in Claude Code's public docs (risk in DESIGN §28).

use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::time::Duration;

use base64::Engine;
use serde::Deserialize;
use serde_json::Value;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixStream;

use super::PastedImage;
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

/// Image `n` pasted into the session, read from its transcript
/// `~/.claude/projects/<dir>/<session>.jsonl`. The transcript format is not
/// in Claude Code's public docs (risk in DESIGN §28).
pub fn pasted_image(session_id: &str, n: u32) -> Option<PastedImage> {
    let name = format!("{session_id}.jsonl");
    let path = std::fs::read_dir(claude_dir().join("projects"))
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
