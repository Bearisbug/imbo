//! Codex: sessions loaded in the shared app-server count as online (ADR-005).
//! Busy sessions get `turn/steer`, idle ones `turn/start`; sessions not on
//! the shared app-server get `codex queue`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use futures::future::BoxFuture;
use futures::{SinkExt, StreamExt};
use imbo_proto::api::MessageView;
use imbo_proto::local::LocalFile;
use serde_json::{Map, Value, json};
use tokio::net::UnixStream;
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tracing::{info, warn};

use super::{Adapter, Connected, Found, Install, Paste, PastedImage, env, on_path, run};
use crate::paths::{Home, exe, tilde, user_home};
use crate::render;

const CALL_TIMEOUT: Duration = Duration::from_secs(5);
/// A session seen only through its own tool calls stays online this long.
const ADOPTED_TTL: Duration = Duration::from_secs(30 * 60);

pub static CODEX: Codex = Codex {
    state: Mutex::new(State {
        adopted: BTreeMap::new(),
        loaded: BTreeSet::new(),
    }),
};

pub struct Codex {
    state: Mutex<State>,
}

struct State {
    /// Threads that called the daemon, and when they last did.
    adopted: BTreeMap<String, Instant>,
    /// Threads loaded in the shared app-server at the last discovery.
    loaded: BTreeSet<String>,
}

impl Adapter for Codex {
    fn id(&self) -> &'static str {
        imbo_proto::TOOL_CODEX
    }

    fn name(&self) -> &'static str {
        "Codex"
    }

    /// The thread is in each call's `_meta["x-codex-turn-metadata"]`; shell
    /// commands get `CODEX_THREAD_ID`.
    fn session(&self, meta: &[&Map<String, Value>]) -> Option<String> {
        meta.iter()
            .find_map(|m| turn_thread(m))
            .or_else(|| env("CODEX_THREAD_ID"))
    }

    /// Codex's MCP client says `codex-mcp-client` at initialize; its MCP
    /// processes get no session in the environment.
    fn mcp_client(&self) -> Option<&'static str> {
        Some("codex")
    }

    fn env_pass(&self) -> &'static [&'static str] {
        &["CODEX_HOME"]
    }

    fn env_session(&self) -> &'static [&'static str] {
        &["CODEX_THREAD_ID"]
    }

    /// A thread outside the shared app-server, seen through its own calls.
    fn adopt(&self, session_id: &str) -> Option<Found> {
        let mut st = self.state.lock().unwrap();
        st.adopted.insert(session_id.into(), Instant::now());
        Some(Found {
            session_id: session_id.into(),
            title: String::new(),
            online: true,
        })
    }

    fn poll_every(&self) -> Duration {
        Duration::from_secs(5)
    }

    /// Threads loaded in the shared app-server, plus threads that called
    /// within `ADOPTED_TTL`.
    fn discover(&self) -> BoxFuture<'_, Result<Vec<Found>, String>> {
        Box::pin(async move {
            let threads = loaded_threads().await?.unwrap_or_default();
            let mut st = self.state.lock().unwrap();
            st.adopted.retain(|_, t| t.elapsed() < ADOPTED_TTL);
            st.loaded = threads.iter().map(|t| t.id.clone()).collect();
            let mut list: Vec<Found> = st
                .adopted
                .keys()
                .filter(|id| !st.loaded.contains(*id))
                .map(|id| Found {
                    session_id: id.clone(),
                    title: String::new(),
                    online: true,
                })
                .collect();
            list.extend(threads.into_iter().map(|t| Found {
                session_id: t.id,
                title: t.title,
                online: true,
            }));
            Ok(list)
        })
    }

    fn channel(&self, session_id: &str) -> &'static str {
        if self.state.lock().unwrap().loaded.contains(session_id) {
            "inject"
        } else {
            "queue"
        }
    }

    fn deliver<'a>(
        &'a self,
        session_id: &'a str,
        m: &'a MessageView,
        files: &'a [LocalFile],
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let text = render::for_codex(m, files);
            match deliver(session_id, &text).await {
                Ok(how) => {
                    info!(msg = m.id, session = %session_id, how, "delivered into Codex session");
                    Ok(())
                }
                Err(e) => {
                    warn!(msg = m.id, session = %session_id, error = %e, "codex delivery failed");
                    Err(e)
                }
            }
        })
    }

    fn paste(&self) -> Paste {
        Paste::Path(
            " Images the user pasted in this session: the image tag <image … path=…> has the path; put it in files to send it as an attachment.",
        )
    }

    /// Codex names the file of each pasted image in the message, so the
    /// agent sends that path as a file instead.
    fn pasted_image(&self, _session_id: &str, _n: u32) -> Result<PastedImage, String> {
        Err("Images pasted in this session carry their path in the image tag; put that path in files.".into())
    }

    fn find(&self) -> Install {
        on_path("codex")
    }

    fn changes(&self) -> String {
        format!(
            "Codex  {}  [mcp_servers.imbo] with {}",
            tilde(&config_file()),
            keys_text()
        )
    }

    fn connect(&self, home: &Home, bin: &Path) -> Result<Connected, String> {
        add_server(home, bin)?;
        Ok(Connected {
            detail: "MCP server imbo",
            warning: None,
        })
    }

    fn disconnect(&self, _home: &Home, bin: Option<&Path>) {
        if let Some(bin) = bin {
            let _ = run(bin, &["mcp", "remove", "imbo"]);
        }
    }
}

/// `_meta["x-codex-turn-metadata"]`, an object or a JSON string.
fn turn_thread(meta: &Map<String, Value>) -> Option<String> {
    let v = meta.get("x-codex-turn-metadata")?;
    let obj = match v {
        Value::String(s) => serde_json::from_str::<Value>(s).ok()?,
        other => other.clone(),
    };
    obj.get("thread_id")
        .or_else(|| obj.get("threadId"))
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Codex's home directory (`~/.codex`).
fn config_dir() -> PathBuf {
    std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| user_home().join(".codex"))
}

fn config_file() -> PathBuf {
    config_dir().join("config.toml")
}

fn add_server(home: &Home, codex: &Path) -> Result<(), String> {
    let _ = run(codex, &["mcp", "remove", "imbo"]);
    let exe = exe().to_string_lossy().into_owned();
    let env = format!("IMBO_HOME={}", home.dir.display());
    let host = format!("IMBO_HOST={}", imbo_proto::TOOL_CODEX);
    let mut args = vec!["mcp", "add", "imbo", "--env", host.as_str()];
    if !home.is_primary() {
        args.extend(["--env", env.as_str()]);
    }
    args.extend(["--", exe.as_str(), "mcp"]);
    run(codex, &args)?;
    let config = config_file();
    let text = std::fs::read_to_string(&config).map_err(|e| e.to_string())?;
    let with_keys = add_keys(&text);
    if with_keys != text {
        std::fs::write(&config, with_keys).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Keys `codex mcp add` has no flags for: uploads can outlast Codex's default
/// 60 s tool timeout (API-MCP-001), and Imbo's tools run without asking each
/// time (ADR-013).
const CODEX_KEYS: [(&str, &str); 2] = [
    ("tool_timeout_sec", "600"),
    ("default_tools_approval_mode", "\"approve\""),
];

fn keys_text() -> String {
    CODEX_KEYS
        .iter()
        .map(|(k, v)| format!("{k} = {v}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Put the missing `CODEX_KEYS` into the `[mcp_servers.imbo]` table that
/// `codex mcp add` wrote.
fn add_keys(config: &str) -> String {
    let lines: Vec<&str> = config.lines().collect();
    let Some(start) = lines.iter().position(|l| l.trim() == "[mcp_servers.imbo]") else {
        return config.to_string();
    };
    let end = lines[start + 1..]
        .iter()
        .position(|l| l.trim_start().starts_with('['))
        .map_or(lines.len(), |i| start + 1 + i);
    let missing: Vec<String> = CODEX_KEYS
        .iter()
        .filter(|(k, _)| {
            !lines[start + 1..end]
                .iter()
                .any(|l| l.trim_start().starts_with(k))
        })
        .map(|(k, v)| format!("{k} = {v}"))
        .collect();
    if missing.is_empty() {
        return config.to_string();
    }
    let mut out = lines[..=start].to_vec();
    out.extend(missing.iter().map(String::as_str));
    out.extend(&lines[start + 1..]);
    let mut s = out.join("\n");
    if config.ends_with('\n') {
        s.push('\n');
    }
    s
}

pub fn socket_path() -> PathBuf {
    config_dir()
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
struct Thread {
    pub id: String,
    pub title: String,
}

/// Titles Codex writes to `session_index.jsonl` (last entry per thread wins).
fn index_titles() -> HashMap<String, String> {
    let mut out = HashMap::new();
    if let Ok(s) = std::fs::read_to_string(config_dir().join("session_index.jsonl")) {
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
async fn loaded_threads() -> Result<Option<Vec<Thread>>, String> {
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
async fn deliver(thread: &str, text: &str) -> Result<&'static str, String> {
    if super::fault(imbo_proto::TOOL_CODEX) {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_keys_go_into_the_imbo_table() {
        let before = "[mcp_servers.other]\ntool_timeout_sec = 30\n\n[mcp_servers.imbo]\ncommand = \"/bin/imbo\"\nargs = [\"mcp\"]\n";
        let after = add_keys(before);
        assert_eq!(
            after,
            "[mcp_servers.other]\ntool_timeout_sec = 30\n\n[mcp_servers.imbo]\ntool_timeout_sec = 600\ndefault_tools_approval_mode = \"approve\"\ncommand = \"/bin/imbo\"\nargs = [\"mcp\"]\n"
        );
        assert_eq!(add_keys(&after), after);
        assert_eq!(add_keys("model = \"x\"\n"), "model = \"x\"\n");
    }
}
