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

use super::{
    Adapter, Connected, Found, Install, Paste, PastedImage, Registration, env, hook_command,
    on_path, run, server_env,
};
use crate::local::Local;
use crate::paths::{Home, exe, tilde, user_home};
use crate::{render, ui};

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

/// What the session's tool process registered: its Claude Code process,
/// the config directory it keeps its files in, and the messaging socket to
/// insert through.
struct Session {
    pid: u32,
    dir: PathBuf,
    socket: Option<String>,
    token: Option<String>,
    failures: u32,
}

/// Liveness and title of a session's Claude Code process.
fn found(session_id: &str, pid: u32, dir: &Path) -> Found {
    let online = alive(pid);
    let title = if online {
        title(pid, dir).unwrap_or_default()
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
            config_dir: Some(config_dir().to_string_lossy().into_owned()),
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
            dir: r.config_dir.map(PathBuf::from).unwrap_or_else(config_dir),
            socket: r.messaging_socket,
            token: r.messaging_token,
            failures,
        };
        let f = found(&r.session_id, session.pid, &session.dir);
        if map.get(&r.session_id).map(|s| &s.dir) != Some(&session.dir) {
            remember_dir(&r.session_id, &session.dir);
        }
        map.insert(r.session_id, session);
        Some(f)
    }

    /// A terminal command inside a session that hasn't registered: found
    /// through `~/.claude/sessions`, with no socket to insert through.
    fn adopt(&self, session_id: &str) -> Option<Found> {
        let mut map = self.sessions.lock().unwrap();
        if !map.contains_key(session_id) {
            let session = Session {
                pid: pid_for(session_id)?,
                dir: config_dir(),
                socket: None,
                token: None,
                failures: 0,
            };
            map.insert(session_id.into(), session);
        }
        let s = &map[session_id];
        Some(found(session_id, s.pid, &s.dir))
    }

    fn poll_every(&self) -> Duration {
        Duration::from_millis(2500)
    }

    /// Re-check the registered processes and titles; closed ones are
    /// forgotten.
    fn discover(&self) -> BoxFuture<'_, Result<Vec<Found>, String>> {
        let mut map = self.sessions.lock().unwrap();
        map.retain(|_, s| alive(s.pid));
        let list = map.iter().map(|(id, s)| found(id, s.pid, &s.dir)).collect();
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
        Paste::Numbered(
            " Images the user pasted in this session ([Image #N]): put the numbers in images to send them as attachments.",
        )
    }

    fn pasted_image(&self, session_id: &str, n: u32) -> Result<PastedImage, String> {
        transcript_image(session_id, n).ok_or_else(|| {
            format!("Couldn't find the pasted image [Image #{n}] in the session log; ask the user to paste it again, then send.")
        })
    }

    fn find(&self) -> Install {
        on_path("claude")
    }

    fn changes(&self) -> String {
        format!(
            "Claude Code  {}  plugin imbo; {CLAUDE_RULE} in permissions.allow",
            tilde(&settings())
        )
    }

    /// The plugin brings the MCP server and the prompt hook; the allow rule
    /// lets its tools run without asking (ADR-013).
    fn connect(&self, home: &Home, bin: &Path) -> Result<Connected, String> {
        add_plugin(home, bin)?;
        let warning = allow_tools(true).err().map(|e| {
            format!(
                "Couldn't edit {} ({e})  {}",
                tilde(&settings()),
                ui::dim(format!(
                    "add \"{CLAUDE_RULE}\" to permissions.allow there, or Claude Code asks before each Imbo tool"
                ))
            )
        });
        Ok(Connected {
            detail: "plugin imbo: tools + prompt hook",
            warning,
        })
    }

    fn disconnect(&self, home: &Home, bin: Option<&Path>) {
        if let Some(bin) = bin {
            remove_plugin(home, bin);
        }
        let _ = allow_tools(false);
    }
}

/// Directory holding the Claude Code state we read (`~/.claude`).
fn config_dir() -> PathBuf {
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

/// Session title for a running Claude Code process, from its config
/// directory `dir`. After /clear or /resume the process holds another
/// session ID than the one registered; the title is still the window's.
fn title(pid: u32, dir: &Path) -> Option<String> {
    let f = read(&dir.join("sessions").join(format!("{pid}.json")))?;
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

/// The config directory each session registered from, one `<session id>
/// <dir>` line each, so a closed session's record can be found again
/// (FLOW-WAKE).
fn dirs_file() -> PathBuf {
    crate::paths::Home::resolve().state().join("claude-dirs")
}

fn remember_dir(session_id: &str, dir: &Path) {
    let path = dirs_file();
    let known = std::fs::read_to_string(&path).unwrap_or_default();
    let line = format!("{session_id} {}", dir.to_string_lossy());
    if !known.lines().any(|l| l == line) {
        let _ = std::fs::create_dir_all(path.parent().unwrap_or(Path::new(".")));
        let _ = std::fs::write(&path, format!("{known}{line}\n"));
    }
}

/// `~/.claude`, where Claude Code keeps its files when `CLAUDE_CONFIG_DIR`
/// isn't set.
fn default_dir() -> PathBuf {
    user_home().join(".claude")
}

/// Where to look for a session's record: the directory it registered from
/// first, since several config directories can share one `projects` folder
/// (profile switchers link theirs to `~/.claude`'s); then the default, the
/// daemon's own, and the others sessions registered from.
fn search_dirs(session_id: &str, known: &str, default: PathBuf, own: PathBuf) -> Vec<PathBuf> {
    let known: Vec<_> = known.lines().filter_map(|l| l.split_once(' ')).collect();
    let mut dirs: Vec<PathBuf> = known
        .iter()
        .rev()
        .find(|(s, _)| *s == session_id)
        .map(|(_, d)| PathBuf::from(d))
        .into_iter()
        .collect();
    dirs.extend([default, own]);
    dirs.extend(known.iter().map(|(_, d)| PathBuf::from(d)));
    dirs
}

/// The config directory and transcript of a session, closed or not.
fn find_transcript(session_id: &str) -> Option<(PathBuf, PathBuf)> {
    let known = std::fs::read_to_string(dirs_file()).unwrap_or_default();
    let dirs = search_dirs(session_id, &known, default_dir(), config_dir());
    let name = format!("{session_id}.jsonl");
    let mut seen = std::collections::HashSet::new();
    for dir in dirs.into_iter().filter(|d| seen.insert(d.clone())) {
        let Ok(projects) = std::fs::read_dir(dir.join("projects")) else {
            continue;
        };
        for p in projects.filter_map(|e| e.ok()) {
            let path = p.path().join(&name);
            if path.is_file() {
                return Some((dir, path));
            }
        }
    }
    None
}

/// The folder a session started in (the first `cwd` in its transcript) and
/// its permission mode (the last `permissionMode`).
fn transcript_start(path: &Path) -> Option<(String, Option<String>)> {
    let file = std::fs::File::open(path).ok()?;
    let (mut cwd, mut mode) = (None, None);
    for line in std::io::BufReader::new(file).lines() {
        let Ok(line) = line else { break };
        let wanted =
            (cwd.is_none() && line.contains("\"cwd\"")) || line.contains("\"permissionMode\"");
        if !wanted {
            continue;
        }
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if cwd.is_none() {
            cwd = v.get("cwd").and_then(Value::as_str).map(String::from);
        }
        if let Some(m) = v.get("permissionMode").and_then(Value::as_str) {
            mode = Some(m.to_string());
        }
    }
    Some((cwd?, mode))
}

/// Whether a running Claude Code process in `dir` has the session open.
fn open_in(dir: &Path, session_id: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(dir.join("sessions")) else {
        return false;
    };
    entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "json"))
        .filter_map(|e| read(&e.path()))
        .any(|f| f.session_id.as_deref() == Some(session_id) && f.pid.is_some_and(alive))
}

fn claude_bin() -> Option<PathBuf> {
    crate::paths::which("claude").or_else(|| {
        let home = user_home();
        [
            home.join(".local/bin/claude"),
            PathBuf::from("/opt/homebrew/bin/claude"),
            PathBuf::from("/usr/local/bin/claude"),
            home.join(".claude/local/claude"),
        ]
        .into_iter()
        .find(|p| p.is_file())
    })
}

/// FLOW-WAKE: open a closed session in the background with `claude --bg
/// --resume`, in the folder it started in, with its config directory and
/// permission mode (ADR-049). Returns once `claude` has handed the session
/// to Claude Code's background service; the error is the reason to show.
pub fn wake(session_id: &str) -> Result<(), String> {
    const NO_RECORD: &str = "No record of this session on this computer.";
    let (dir, transcript) = find_transcript(session_id).ok_or(NO_RECORD)?;
    let (cwd, mode) = transcript_start(&transcript).ok_or(NO_RECORD)?;
    if !Path::new(&cwd).is_dir() {
        return Err(format!("Its folder {cwd} is gone."));
    }
    if open_in(&dir, session_id) {
        return Err("It's open in a window that isn't connected to Imbo.".into());
    }
    let claude = claude_bin().ok_or("Claude Code isn't installed where Imbo can find it.")?;
    let log = crate::paths::Home::resolve()
        .state()
        .join(format!("wake-{session_id}.log"));
    let out = std::fs::File::create(&log).map_err(|e| format!("Imbo couldn't open it: {e}"))?;
    let err = out
        .try_clone()
        .map_err(|e| format!("Imbo couldn't open it: {e}"))?;
    let mut cmd = std::process::Command::new(claude);
    cmd.args(["--bg", "--resume", session_id])
        .current_dir(&cwd)
        .stdin(std::process::Stdio::null())
        .stdout(out)
        .stderr(err);
    if let Some(m) = &mode {
        cmd.args(["--permission-mode", m]);
    }
    // Test sessions load the plugin from a folder rather than the installed one.
    if let Some(p) = env("IMBO_WAKE_PLUGIN_DIR") {
        cmd.arg("--plugin-dir").arg(p);
    }
    if dir == default_dir() {
        cmd.env_remove("CLAUDE_CONFIG_DIR");
    } else {
        cmd.env("CLAUDE_CONFIG_DIR", &dir);
    }
    for k in [
        "CLAUDE_CODE_SESSION_ID",
        "CLAUDE_CODE_MESSAGING_SOCKET",
        "CLAUDE_CODE_MESSAGING_TOKEN",
    ] {
        cmd.env_remove(k);
    }
    let mut child = cmd
        .spawn()
        .map_err(|e| format!("Claude Code couldn't open it: {e}"))?;
    let start = std::time::Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break s,
            Ok(None) if start.elapsed() < Duration::from_secs(20) => {
                std::thread::sleep(Duration::from_millis(200))
            }
            _ => {
                let _ = child.kill();
                let _ = std::fs::remove_file(&log);
                return Err(
                    "Claude Code couldn't open it: claude didn't finish within 20 seconds.".into(),
                );
            }
        }
    };
    let text = std::fs::read_to_string(&log).unwrap_or_default();
    let _ = std::fs::remove_file(&log);
    info!(
        session = session_id,
        cwd,
        mode = mode.as_deref().unwrap_or(""),
        ok = status.success(),
        "claude --bg --resume"
    );
    if status.success() {
        return Ok(());
    }
    if text.to_lowercase().contains("not trusted") {
        return Err(format!(
            "Claude Code doesn't trust the folder {cwd} yet; open Claude Code there once."
        ));
    }
    let first = text
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("it exited with an error");
    Err(format!("Claude Code couldn't open it: {first}"))
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

/// Where `imbo setup` keeps the plugin marketplace it registers.
fn plugin_dir(home: &Home) -> PathBuf {
    home.dir.join("claude-plugin")
}

/// Write a Claude Code plugin (MCP server + prompt hook) at `root`.
pub fn write_plugin(home: &Home, root: &Path) -> anyhow::Result<()> {
    let exe = exe().to_string_lossy().into_owned();
    std::fs::create_dir_all(root.join(".claude-plugin"))?;
    std::fs::create_dir_all(root.join("hooks"))?;
    let manifest = json!({
        "name": "imbo",
        "version": crate::VERSION,
        "description": "Messages between agent sessions, across people and computers.",
        "author": { "name": "Imbo" },
        "homepage": "https://imbo.dev",
        "license": "Apache-2.0"
    });
    std::fs::write(
        root.join(".claude-plugin/plugin.json"),
        serde_json::to_string_pretty(&manifest)?,
    )?;
    let server = json!({
        "command": exe,
        "args": ["mcp"],
        "env": server_env(home, imbo_proto::TOOL_CLAUDE),
    });
    let hook_cmd = hook_command(home, "hook prompt");
    let mcp = json!({ "mcpServers": { "imbo": server } });
    std::fs::write(root.join(".mcp.json"), serde_json::to_string_pretty(&mcp)?)?;
    let hooks = json!({
        "hooks": {
            "UserPromptSubmit": [
                { "hooks": [ { "type": "command", "command": hook_cmd, "timeout": 10 } ] }
            ]
        }
    });
    std::fs::write(
        root.join("hooks/hooks.json"),
        serde_json::to_string_pretty(&hooks)?,
    )?;
    Ok(())
}

/// The local marketplace `imbo setup` registers with Claude Code.
fn write_marketplace(home: &Home) -> anyhow::Result<PathBuf> {
    let dir = plugin_dir(home);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join(".claude-plugin"))?;
    let market = json!({
        "name": marketplace_name(home),
        "owner": { "name": "Imbo" },
        "plugins": [ {
            "name": "imbo",
            "source": "./plugin",
            "description": "Messages between agent sessions, across people and computers.",
            "version": crate::VERSION
        } ]
    });
    std::fs::write(
        dir.join(".claude-plugin/marketplace.json"),
        serde_json::to_string_pretty(&market)?,
    )?;
    write_plugin(home, &dir.join("plugin"))?;
    Ok(dir)
}

fn marketplace_name(home: &Home) -> String {
    if home.is_primary() {
        "imbo".into()
    } else {
        home.label().replace("dev.imbo.daemon.", "imbo-")
    }
}

fn add_plugin(home: &Home, claude: &Path) -> Result<(), String> {
    let dir = write_marketplace(home).map_err(|e| e.to_string())?;
    let market = marketplace_name(home);
    let _ = run(claude, &["plugin", "marketplace", "remove", &market]);
    run(
        claude,
        &["plugin", "marketplace", "add", &dir.to_string_lossy()],
    )?;
    run(
        claude,
        &[
            "plugin",
            "install",
            &format!("imbo@{market}"),
            "--scope",
            "user",
        ],
    )?;
    Ok(())
}

fn remove_plugin(home: &Home, claude: &Path) {
    let market = marketplace_name(home);
    let _ = run(
        claude,
        &[
            "plugin",
            "uninstall",
            &format!("imbo@{market}"),
            "--scope",
            "user",
        ],
    );
    let _ = run(claude, &["plugin", "marketplace", "remove", &market]);
    let _ = std::fs::remove_dir_all(plugin_dir(home));
}

/// Claude Code permission rule covering every tool of the imbo plugin's MCP server.
const CLAUDE_RULE: &str = "mcp__plugin_imbo_imbo";

fn settings() -> PathBuf {
    config_dir().join("settings.json")
}

/// Claude Code settings text with `CLAUDE_RULE` added to (or removed from)
/// `permissions.allow`, everything else kept; `None` when nothing changes.
fn edit_allow(text: &str, add: bool) -> Result<Option<String>, String> {
    let mut v: Value = if text.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(text).map_err(|e| format!("not valid JSON: {e}"))?
    };
    let root = v.as_object_mut().ok_or("not a JSON object")?;
    if !add && !root.contains_key("permissions") {
        return Ok(None);
    }
    let perms = root
        .entry("permissions")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or("`permissions` isn't an object")?;
    if !add && !perms.contains_key("allow") {
        return Ok(None);
    }
    let allow = perms
        .entry("allow")
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .ok_or("`permissions.allow` isn't a list")?;
    match (add, allow.iter().any(|r| r == CLAUDE_RULE)) {
        (true, false) => allow.push(json!(CLAUDE_RULE)),
        (false, true) => allow.retain(|r| r != CLAUDE_RULE),
        _ => return Ok(None),
    }
    let mut out = serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?;
    out.push('\n');
    Ok(Some(out))
}

/// Let Claude Code run Imbo's tools without asking each time (ADR-013), or
/// take that back.
fn allow_tools(add: bool) -> Result<(), String> {
    let path = settings();
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.to_string()),
    };
    if let Some(new) = edit_allow(&text, add)? {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        std::fs::write(&path, new).map_err(|e| e.to_string())?;
    }
    Ok(())
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
    let input = serde_json::from_str::<Value>(&input).unwrap_or_default();
    // Cursor runs this plugin's hook too, and ZCode sets Claude Code's
    // session variable for its hooks; both have hooks of their own (ADR-018).
    if input.get("cursor_version").is_some()
        || ["CURSOR_VERSION", "ZCODE_SESSION_ID", "ZCODE_RUNTIME_ENV"]
            .iter()
            .any(|v| env(v).is_some())
    {
        return Ok(());
    }
    let session_id = input["session_id"]
        .as_str()
        .map(str::to_string)
        .or_else(|| env("CLAUDE_CODE_SESSION_ID"));
    let Some(session_id) = session_id else {
        return Ok(());
    };
    let req = LocalRequest::HookPrompt {
        tool: imbo_proto::TOOL_CLAUDE.into(),
        session_id,
        limit: None,
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
    fn title_is_read_from_the_sessions_own_dir_by_process() {
        let dir = std::env::temp_dir().join(format!("imbo-title-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sessions")).unwrap();
        let file = dir.join("sessions/4242.json");
        // The process moved on to another session ID after /clear.
        std::fs::write(
            &file,
            r#"{"pid":4242,"sessionId":"after-clear","name":"Dotra Akamai"}"#,
        )
        .unwrap();
        assert_eq!(title(4242, &dir).as_deref(), Some("Dotra Akamai"));
        assert_eq!(title(4242, &dir.join("other")), None);
        std::fs::write(
            &file,
            r#"{"pid":4242,"sessionId":"after-clear","name":" "}"#,
        )
        .unwrap();
        assert_eq!(title(4242, &dir), None);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_session_is_looked_for_first_where_it_registered() {
        let known = "a /p/lai\nb /p/other dir\na /p/work\n";
        let dirs = |s| search_dirs(s, known, "/h/.claude".into(), "/h/own".into());
        let d: Vec<PathBuf> = [
            "/p/work",
            "/h/.claude",
            "/h/own",
            "/p/lai",
            "/p/other dir",
            "/p/work",
        ]
        .map(PathBuf::from)
        .into();
        assert_eq!(dirs("a"), d);
        assert_eq!(dirs("c")[0], PathBuf::from("/h/.claude"));
        assert_eq!(dirs("b")[0], PathBuf::from("/p/other dir"));
    }

    #[test]
    fn older_tool_processes_register_without_a_config_dir() {
        let line = r#"{"op":"register","tool":"claude-code","session_id":"s","messaging_socket":null,"messaging_token":null,"pid":7}"#;
        let req: LocalRequest = serde_json::from_str(line).unwrap();
        assert!(matches!(
            req,
            LocalRequest::Register {
                config_dir: None,
                ..
            }
        ));
    }

    #[test]
    fn claude_allow_rule_is_added_once_and_removed() {
        let before =
            r#"{"model":"sonnet","permissions":{"allow":["Bash(ls:*)"],"deny":[]},"hooks":{}}"#;
        let added = edit_allow(before, true).unwrap().unwrap();
        let v: Value = serde_json::from_str(&added).unwrap();
        assert_eq!(
            v["permissions"]["allow"],
            json!(["Bash(ls:*)", "mcp__plugin_imbo_imbo"])
        );
        let keys: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys, ["model", "permissions", "hooks"]);
        assert_eq!(edit_allow(&added, true).unwrap(), None);

        let removed = edit_allow(&added, false).unwrap().unwrap();
        let v: Value = serde_json::from_str(&removed).unwrap();
        assert_eq!(v, serde_json::from_str::<Value>(before).unwrap());
        assert_eq!(edit_allow(&removed, false).unwrap(), None);

        let fresh = edit_allow("", true).unwrap().unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&fresh).unwrap(),
            json!({ "permissions": { "allow": ["mcp__plugin_imbo_imbo"] } })
        );
        assert_eq!(edit_allow("", false).unwrap(), None);
        assert!(edit_allow("{ not json", true).is_err());
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
