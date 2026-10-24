//! Codex: sessions loaded in the shared app-server count as online (ADR-005).
//! Busy sessions get `turn/steer`, idle ones `turn/start`; sessions not on
//! the shared app-server get `codex queue`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

use futures::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::net::UnixStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;

use crate::paths::codex_home;

const CALL_TIMEOUT: Duration = Duration::from_secs(5);

pub fn socket_path() -> PathBuf {
    codex_home()
        .join("app-server-control")
        .join("app-server-control.sock")
}

pub struct Rpc {
    ws: WebSocketStream<UnixStream>,
    next: u64,
}

impl Rpc {
    pub async fn connect() -> Result<Self, String> {
        let path = socket_path();
        let stream = tokio::time::timeout(Duration::from_secs(2), UnixStream::connect(&path))
            .await
            .map_err(|_| "connect timed out".to_string())?
            .map_err(|e| format!("{}: {e}", path.display()))?;
        let (ws, _) = tokio::time::timeout(
            CALL_TIMEOUT,
            tokio_tungstenite::client_async("ws://localhost/", stream),
        )
        .await
        .map_err(|_| "handshake timed out".to_string())?
        .map_err(|e| format!("handshake: {e}"))?;
        let mut rpc = Rpc { ws, next: 1 };
        rpc.call(
            "initialize",
            json!({ "clientInfo": { "name": "imbo", "version": crate::VERSION } }),
        )
        .await?;
        rpc.notify("initialized").await?;
        Ok(rpc)
    }

    async fn notify(&mut self, method: &str) -> Result<(), String> {
        let text = json!({ "method": method }).to_string();
        self.ws
            .send(Message::Text(text.into()))
            .await
            .map_err(|e| e.to_string())
    }

    pub async fn call(&mut self, method: &str, params: Value) -> Result<Value, String> {
        let id = self.next;
        self.next += 1;
        let text = json!({ "id": id, "method": method, "params": params }).to_string();
        self.ws
            .send(Message::Text(text.into()))
            .await
            .map_err(|e| e.to_string())?;
        let wait = async {
            while let Some(msg) = self.ws.next().await {
                let msg = msg.map_err(|e| e.to_string())?;
                let Message::Text(t) = msg else { continue };
                let Ok(v) = serde_json::from_str::<Value>(&t) else {
                    continue;
                };
                if v.get("id").and_then(Value::as_u64) != Some(id) {
                    continue;
                }
                if let Some(err) = v.get("error") {
                    return Err(format!("{method}: {err}"));
                }
                return Ok(v.get("result").cloned().unwrap_or(Value::Null));
            }
            Err("app-server closed the connection".to_string())
        };
        tokio::time::timeout(CALL_TIMEOUT, wait)
            .await
            .map_err(|_| format!("{method} timed out"))?
    }

    pub async fn close(mut self) {
        let _ = self.ws.close(None).await;
    }

    async fn loaded_ids(&mut self) -> Result<Vec<String>, String> {
        let mut ids = vec![];
        let mut cursor: Option<String> = None;
        for _ in 0..20 {
            let r = self
                .call("thread/loaded/list", json!({ "cursor": cursor }))
                .await?;
            if let Some(data) = r.get("data").and_then(Value::as_array) {
                ids.extend(data.iter().filter_map(|v| v.as_str().map(str::to_string)));
            }
            cursor = r
                .get("nextCursor")
                .and_then(Value::as_str)
                .map(str::to_string);
            if cursor.is_none() {
                break;
            }
        }
        Ok(ids)
    }
}

#[derive(Debug, Clone)]
pub struct Thread {
    pub id: String,
    pub title: String,
}

/// Titles Codex writes to `session_index.jsonl` (last entry per thread wins).
fn index_titles() -> HashMap<String, String> {
    let mut out = HashMap::new();
    if let Ok(s) = std::fs::read_to_string(codex_home().join("session_index.jsonl")) {
        for line in s.lines() {
            if let Ok(v) = serde_json::from_str::<Value>(line)
                && let (Some(id), Some(name)) = (v["id"].as_str(), v["thread_name"].as_str())
            {
                out.insert(id.to_string(), name.to_string());
            }
        }
    }
    out
}

/// Top-level threads loaded in the shared app-server, with titles.
/// `Ok(None)` when the app-server isn't running.
pub async fn loaded_threads() -> Result<Option<Vec<Thread>>, String> {
    if !socket_path().exists() {
        return Ok(None);
    }
    let mut rpc = match Rpc::connect().await {
        Ok(r) => r,
        Err(_) => return Ok(None),
    };
    let ids = rpc.loaded_ids().await?;
    let titles = index_titles();
    let mut out = vec![];
    for id in ids {
        let Ok(r) = rpc.call("thread/read", json!({ "threadId": id })).await else {
            continue;
        };
        let t = &r["thread"];
        if !t["parentThreadId"].is_null() || t["ephemeral"].as_bool() == Some(true) {
            continue;
        }
        let title = t["name"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .map(str::to_string)
            .or_else(|| titles.get(&id).cloned())
            .or_else(|| {
                t["preview"]
                    .as_str()
                    .filter(|s| !s.trim().is_empty())
                    .map(|s| s.chars().take(60).collect())
            })
            .unwrap_or_default();
        out.push(Thread { id, title });
    }
    rpc.close().await;
    Ok(Some(out))
}

/// Deliver into one thread. Returns how ("steer", "start" or "queue").
pub async fn deliver(thread: &str, text: &str) -> Result<&'static str, String> {
    if super::fault("codex") {
        return Err("fault injected".into());
    }
    let input = json!([{ "type": "text", "text": text, "text_elements": [] }]);
    if socket_path().exists()
        && let Ok(mut rpc) = Rpc::connect().await
    {
        let loaded = rpc.loaded_ids().await.unwrap_or_default();
        if loaded.iter().any(|id| id == thread) {
            let read = rpc
                .call("thread/read", json!({ "threadId": thread }))
                .await?;
            let active = read["thread"]["status"]["type"].as_str() == Some("active");
            if active
                && let Ok(turns) = rpc
                    .call(
                        "thread/turns/list",
                        json!({ "threadId": thread, "limit": 1, "itemsView": "notLoaded" }),
                    )
                    .await
            {
                let current = turns["data"]
                    .as_array()
                    .and_then(|d| {
                        d.iter()
                            .find(|t| t["status"].as_str() == Some("inProgress"))
                    })
                    .and_then(|t| t["id"].as_str())
                    .map(str::to_string);
                if let Some(turn) = current {
                    let r = rpc
                        .call(
                            "turn/steer",
                            json!({ "threadId": thread, "expectedTurnId": turn, "input": input }),
                        )
                        .await;
                    if r.is_ok() {
                        rpc.close().await;
                        return Ok("steer");
                    }
                }
            }
            let r = rpc
                .call("turn/start", json!({ "threadId": thread, "input": input }))
                .await;
            rpc.close().await;
            return r.map(|_| "start");
        }
        rpc.close().await;
    }
    queue(thread, text).await
}

async fn queue(thread: &str, text: &str) -> Result<&'static str, String> {
    let mut cmd = tokio::process::Command::new("codex");
    cmd.args(["queue", "--thread", thread, "--message", text])
        .kill_on_drop(true);
    let out = tokio::time::timeout(Duration::from_secs(30), cmd.output())
        .await
        .map_err(|_| "codex queue timed out".to_string())?
        .map_err(|e| format!("codex queue: {e}"))?;
    if out.status.success() {
        Ok("queue")
    } else {
        let mut msg = String::from_utf8_lossy(&out.stderr).to_string();
        msg.push_str(&String::from_utf8_lossy(&out.stdout));
        Err(format!(
            "codex queue failed: {}",
            msg.trim().chars().take(300).collect::<String>()
        ))
    }
}
