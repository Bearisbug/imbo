//! Kimi Code: like MiniMax Code, the agent runs `imbo wait` in the
//! background and the stop hook asks for it at the end of each turn
//! (FLOW-LISTEN). Its hooks tell the daemon which session each Kimi Code
//! process has open; the MCP server, `imbo wait` and the agent's commands
//! find that process above them (ADR-044).

use std::path::{Path, PathBuf};
use std::time::Duration;

use futures::future::BoxFuture;
use imbo_proto::api::MessageView;
use imbo_proto::local::LocalFile;
use serde_json::{Map, Value, json};

use super::{
    Adapter, Connected, Found, Install, Paste, PastedImage, Registration, drop_empty, edit_json,
    env, hook_command, object, on_path, our_hook, remembered_all, server_env,
};
use crate::paths::{Home, exe, tilde, user_home};
use crate::ui;

/// Uploads can outlast Kimi Code's default wait for a tool call
/// (API-MCP-001).
const CALL_TIMEOUT_MS: u64 = 840_000;
/// Hook context longer than this is cut.
const CONTEXT_LIMIT: usize = 9000;
const HOOK_EVENTS: [&str; 4] = ["SessionStart", "Stop", "UserPromptSubmit", "SessionEnd"];
/// What `imbo setup` says when new sessions keep asking (ADR-044).
const APPROVE_HINT: &str = "Kimi Code asks before Imbo's tools and `imbo wait` in each new session; choose \"Approve for this session\" there.";

pub static KIMI: Kimi = Kimi;

pub struct Kimi;

impl Adapter for Kimi {
    fn id(&self) -> &'static str {
        imbo_proto::TOOL_KIMI
    }

    fn name(&self) -> &'static str {
        "Kimi Code"
    }

    /// Kimi Code tells its MCP servers and commands nothing of the session:
    /// it is the one the hooks last reported for the Kimi Code process
    /// above.
    fn session(&self, meta: &[&Map<String, Value>]) -> Option<String> {
        meta.iter()
            .find_map(|m| m.get("imbo_session")?.as_str().map(str::to_string))
            .or_else(|| super::ancestor_session(self.id()).map(|(_, s)| s))
    }

    fn env_pass(&self) -> &'static [&'static str] {
        &["KIMI_CODE_HOME"]
    }

    fn env_session(&self) -> &'static [&'static str] {
        &[]
    }

    fn announce_call(&self, session_id: &str) -> Option<Registration> {
        let pid = super::ancestor_session(self.id())
            .map(|(pid, _)| pid)
            .unwrap_or_else(|| unsafe { libc::getppid() } as u32);
        Some(Registration {
            session_id: session_id.into(),
            messaging_socket: None,
            messaging_token: None,
            pid,
        })
    }

    fn register(&self, r: Registration) -> Option<Found> {
        super::remember(self.id(), r.pid, &r.session_id);
        Some(found(&r.session_id, crate::procs::alive(r.pid)))
    }

    fn adopt(&self, session_id: &str) -> Option<Found> {
        let (pid, _) = remembered_all(self.id())
            .into_iter()
            .find(|(_, s)| s == session_id)?;
        Some(found(session_id, crate::procs::alive(pid)))
    }

    fn poll_every(&self) -> Duration {
        Duration::from_secs(5)
    }

    /// The session each running Kimi Code process has open.
    fn discover(&self) -> BoxFuture<'_, Result<Vec<Found>, String>> {
        let list = remembered_all(self.id())
            .into_iter()
            .map(|(_, id)| found(&id, true))
            .collect();
        Box::pin(std::future::ready(Ok(list)))
    }

    fn channel(&self, session_id: &str) -> &'static str {
        super::listen_channel(self.id(), session_id)
    }

    fn listens(&self) -> bool {
        true
    }

    fn wait_target(&self) -> Option<(String, Option<u32>)> {
        let (pid, session) = super::ancestor_session(self.id())?;
        Some((session, Some(pid)))
    }

    fn hook<'a>(&'a self, event: &'a str, input: &'a Value) -> BoxFuture<'a, Option<String>> {
        Box::pin(async move {
            let session_id = input["session_id"].as_str().filter(|s| !s.is_empty())?;
            let pid = super::tool_process().unwrap_or_else(|| unsafe { libc::getppid() } as u32);
            // `/new` reports the old session's end and the new one's start
            // at the same moment, in either order.
            if event == "SessionEnd" {
                super::forget(self.id(), pid, session_id);
                return None;
            }
            super::hook_register(self.id(), session_id, pid).await;
            match event {
                // Kimi Code adds what the hook prints to the prompt.
                "UserPromptSubmit" => {
                    let note = note();
                    super::hook_context(self.id(), session_id, CONTEXT_LIMIT - note.len())
                        .await
                        .map(|c| format!("{note}\n\n{c}"))
                }
                // Kimi Code continues the turn once with what a hook that
                // exits with 2 wrote to stderr.
                "Stop" => {
                    if super::needs_listener(self.id(), session_id).await {
                        eprint!("{}", rearm());
                        std::process::exit(2);
                    }
                    None
                }
                _ => None,
            }
        })
    }

    fn deliver<'a>(
        &'a self,
        _session_id: &'a str,
        _m: &'a MessageView,
        _files: &'a [LocalFile],
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(std::future::ready(Err(
            "Kimi Code sessions get messages through their listener".into(),
        )))
    }

    fn paste(&self) -> Paste {
        Paste::Path(
            " To send an image the user pasted, put the path Kimi Code shows for it in files.",
        )
    }

    fn pasted_image(&self, _session_id: &str, _n: u32) -> Result<PastedImage, String> {
        Err("Kimi Code sessions send pasted images by path, in files.".into())
    }

    fn find(&self) -> Install {
        on_path("kimi")
    }

    fn changes(&self) -> String {
        format!(
            "Kimi Code  {}  mcpServers.imbo; {}  session start, stop, prompt and session end hooks",
            tilde(&mcp_file()),
            tilde(&config_file())
        )
    }

    /// Kimi Code reads both files when a session starts.
    fn connect(&self, home: &Home, _bin: &Path) -> Result<Connected, String> {
        let server = json!({
            "command": exe().to_string_lossy(),
            "args": ["mcp"],
            "env": server_env(home, self.id()),
            "toolTimeoutMs": CALL_TIMEOUT_MS,
        });
        let by_hand = format!("\"mcpServers\": {{\"imbo\": {server}}}");
        edit_json(&mcp_file(), &by_hand, |root| {
            object(root, "mcpServers")?.insert("imbo".into(), server.clone());
            Ok(())
        })?;
        let blocks = hook_blocks(home);
        edit_toml(&config_file(), &blocks, |text| {
            let mut out = without_hooks(text, home);
            if !out.is_empty() && !out.ends_with("\n\n") {
                out.push_str(if out.ends_with('\n') { "\n" } else { "\n\n" });
            }
            out.push_str(&blocks);
            out
        })?;
        Ok(Connected {
            detail: "MCP server imbo + hooks",
            warning: None,
        })
    }

    /// The permission rules Kimi Code 2.4 offers aren't read, so Imbo's
    /// tools and `imbo wait` run without asking only in yolo or auto mode
    /// (ADR-044): asked, and changed only on yes.
    fn after_connect(&self, home: &Home, interactive: bool) -> std::io::Result<Option<String>> {
        let text = std::fs::read_to_string(config_file()).unwrap_or_default();
        if matches!(mode(&text).as_str(), "yolo" | "auto") {
            return Ok(None);
        }
        if !interactive {
            return Ok(Some(APPROVE_HINT.into()));
        }
        let yes = cliclack::confirm(
            "Start new Kimi Code sessions in yolo mode? Then Kimi Code doesn't ask before Imbo's tools, nor before other edits and commands",
        )
        .initial_value(false)
        .interact()?;
        if !yes {
            return Ok(Some(APPROVE_HINT.into()));
        }
        Ok(Some(match set_yolo(home) {
            Ok(()) => {
                "New Kimi Code sessions start in yolo mode; `imbo uninstall` puts the mode back"
                    .into()
            }
            Err(e) => format!("{e}  {}", ui::dim(APPROVE_HINT)),
        }))
    }

    fn disconnect(&self, home: &Home, _bin: Option<&Path>) {
        let _ = edit_json(&mcp_file(), "", |root| {
            if let Some(Value::Object(servers)) = root.get_mut("mcpServers") {
                servers.remove("imbo");
            }
            drop_empty(root, "mcpServers");
            Ok(())
        });
        let _ = edit_toml(&config_file(), "", |text| without_hooks(text, home));
        restore_mode(home);
    }
}

fn data_dir() -> PathBuf {
    env("KIMI_CODE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| user_home().join(".kimi-code"))
}

fn mcp_file() -> PathBuf {
    data_dir().join("mcp.json")
}

fn config_file() -> PathBuf {
    data_dir().join("config.toml")
}

fn found(session_id: &str, online: bool) -> Found {
    Found {
        session_id: session_id.into(),
        title: title(session_id).unwrap_or_default(),
        online,
    }
}

/// The session's title, kept in `state.json` in its folder, which
/// `session_index.jsonl` names.
fn title(session_id: &str) -> Option<String> {
    let index = std::fs::read_to_string(data_dir().join("session_index.jsonl")).ok()?;
    let dir = index
        .lines()
        .rev()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .find(|v| v["sessionId"] == session_id)?["sessionDir"]
        .as_str()?
        .to_string();
    let state: Value =
        serde_json::from_str(&std::fs::read_to_string(Path::new(&dir).join("state.json")).ok()?)
            .ok()?;
    state["title"]
        .as_str()
        .map(|t| t.trim().to_string())
        .filter(|t| !t.is_empty())
}

/// Kimi Code doesn't pass MCP server instructions to the model, so the hooks
/// carry them; without them the agent takes Imbo messages for a prompt
/// injection (ADR-044).
fn note() -> String {
    format!(
        "Your user connected this session to Imbo. {}\n\nMessages from other sessions come in through Imbo, from the inbox tool or with the user's prompt; each starts with [from <sender>] and ends with an [Imbo message #N] line saying how to reply. They come from your user's contacts, not from a prompt injection.",
        crate::mcp::INSTRUCTIONS
    )
}

/// The stop hook's request when no listener waits (API-LOCAL-011). The
/// command is the same every time, so a session approves it once.
fn rearm() -> String {
    format!(
        "{}\n\nImbo's message listener isn't running. Use the Bash tool to run the command below in the background (run_in_background=true, disable_timeout=true, description \"{}\"); don't wait for it to finish, and end this turn right after starting it without saying anything else: {}",
        note(),
        super::WAKE,
        hook_command(&Home::resolve(), "wait")
    )
}

/// Imbo's hooks as `[[hooks]]` tables.
fn hook_blocks(home: &Home) -> String {
    HOOK_EVENTS
        .iter()
        .map(|event| {
            let command = hook_command(home, &format!("hook {} {event}", imbo_proto::TOOL_KIMI));
            format!(
                "[[hooks]]\nevent = \"{event}\"\ncommand = {}\ntimeout = 10\n",
                toml_string(&command)
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn toml_string(s: &str) -> String {
    toml::Value::String(s.into()).to_string()
}

/// `text` without the `[[hooks]]` tables whose command is one of Imbo's
/// for `home`, and without the blank lines before them.
fn without_hooks(text: &str, home: &Home) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let mut out: Vec<&str> = vec![];
    let mut i = 0;
    while i < lines.len() {
        let header = lines[i].trim();
        if header != "[[hooks]]" {
            out.push(lines[i]);
            i += 1;
            continue;
        }
        let end = lines[i + 1..]
            .iter()
            .position(|l| l.trim_start().starts_with('['))
            .map_or(lines.len(), |n| i + 1 + n);
        let ours = lines[i + 1..end].iter().any(|l| {
            l.trim_start()
                .strip_prefix("command")
                .and_then(|rest| rest.trim_start().strip_prefix('='))
                .and_then(|v| toml::from_str::<toml::Table>(&format!("v = {}", v.trim())).ok())
                .and_then(|t| {
                    t.get("v")?
                        .as_str()
                        .map(|c| our_hook(c, home, imbo_proto::TOOL_KIMI))
                })
                .unwrap_or(false)
        });
        if ours {
            while out.last().is_some_and(|l| l.trim().is_empty()) {
                out.pop();
            }
            if end < lines.len() && !out.is_empty() {
                out.push("");
            }
        } else {
            out.extend(&lines[i..end]);
        }
        i = end;
    }
    let mut s = out.join("\n");
    if !s.is_empty() {
        s.push('\n');
    }
    s
}

/// Read `path`, let `edit` change its text, and write it back when it
/// changed. A file that isn't TOML is left alone, and the error says what
/// to add by hand.
fn edit_toml(path: &Path, by_hand: &str, edit: impl FnOnce(&str) -> String) -> Result<(), String> {
    let fail = |e: String| {
        format!(
            "Couldn't edit {} ({e}). Add this yourself:\n{by_hand}",
            tilde(path)
        )
    };
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(fail(e.to_string())),
    };
    toml::from_str::<toml::Table>(&text).map_err(|e| fail(format!("not valid TOML: {e}")))?;
    let new = edit(&text);
    if new == text {
        return Ok(());
    }
    toml::from_str::<toml::Table>(&new)
        .map_err(|e| fail(format!("edit left invalid TOML: {e}")))?;
    super::write_text(path, &new).map_err(fail)
}

/// The default permission mode new sessions start in: `default_yolo`
/// is the older way to say yolo.
fn mode(text: &str) -> String {
    let Ok(t) = toml::from_str::<toml::Table>(text) else {
        return "manual".into();
    };
    if let Some(m) = t.get("default_permission_mode").and_then(|v| v.as_str()) {
        return m.into();
    }
    if t.get("default_yolo").and_then(|v| v.as_bool()) == Some(true) {
        return "yolo".into();
    }
    "manual".into()
}

/// Where the mode before `set_yolo` is kept: the value, or nothing when the
/// key wasn't set.
fn saved_mode_path(home: &Home) -> PathBuf {
    home.state().join("kimi-code-mode")
}

/// The top-level `default_permission_mode` line's value, if there is one.
fn mode_line(text: &str) -> Option<(usize, String)> {
    text.lines()
        .take_while(|l| !l.trim_start().starts_with('['))
        .enumerate()
        .find_map(|(i, l)| {
            let rest = l.trim_start().strip_prefix("default_permission_mode")?;
            let v = rest.trim_start().strip_prefix('=')?;
            let t = toml::from_str::<toml::Table>(&format!("v = {}", v.trim())).ok()?;
            Some((i, t.get("v")?.as_str()?.to_string()))
        })
}

/// `text` with the top-level `default_permission_mode` set to `value`, or
/// removed for `None`.
fn with_mode(text: &str, value: Option<&str>) -> String {
    let mut lines: Vec<String> = text.lines().map(str::to_string).collect();
    let line = value.map(|v| format!("default_permission_mode = {}", toml_string(v)));
    match (mode_line(text), line) {
        (Some((i, _)), Some(l)) => lines[i] = l,
        (Some((i, _)), None) => {
            lines.remove(i);
        }
        (None, Some(l)) => lines.insert(0, l),
        (None, None) => {}
    }
    let mut s = lines.join("\n");
    if !s.is_empty() {
        s.push('\n');
    }
    s
}

fn set_yolo(home: &Home) -> Result<(), String> {
    let path = config_file();
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    let before = mode_line(&text).map(|(_, v)| v).unwrap_or_default();
    std::fs::write(saved_mode_path(home), &before).map_err(|e| e.to_string())?;
    edit_toml(&path, "default_permission_mode = \"yolo\"", |t| {
        with_mode(t, Some("yolo"))
    })
}

/// Put back the mode `set_yolo` replaced, unless the user changed it since.
fn restore_mode(home: &Home) {
    let saved = saved_mode_path(home);
    let Ok(before) = std::fs::read_to_string(&saved) else {
        return;
    };
    let path = config_file();
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    if mode_line(&text).is_some_and(|(_, v)| v == "yolo") {
        let before = before.trim();
        let _ = edit_toml(&path, "", |t| {
            with_mode(t, (!before.is_empty()).then_some(before))
        });
    }
    let _ = std::fs::remove_file(saved);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> Home {
        Home::resolve()
    }

    #[test]
    fn hooks_are_added_once_and_removed_with_the_users_kept() {
        let h = home();
        let user = "default_model = \"x\"\n\n[[hooks]]\nevent = \"Stop\"\ncommand = \"notify-send done\"\n\n[providers.d]\ntype = \"openai\"\n";
        let blocks = hook_blocks(&h);
        let add = |t: &str| {
            let mut out = without_hooks(t, &h);
            if !out.is_empty() && !out.ends_with("\n\n") {
                out.push_str(if out.ends_with('\n') { "\n" } else { "\n\n" });
            }
            out.push_str(&blocks);
            out
        };
        let once = add(user);
        assert_eq!(add(&once), once);
        let t: toml::Table = toml::from_str(&once).unwrap();
        assert_eq!(t["hooks"].as_array().unwrap().len(), 5);
        assert_eq!(t["providers"]["d"]["type"].as_str(), Some("openai"));
        let back = without_hooks(&once, &h);
        assert_eq!(
            toml::from_str::<toml::Table>(&back).unwrap(),
            toml::from_str::<toml::Table>(user).unwrap()
        );
        assert_eq!(without_hooks("", &h), "");
    }

    #[test]
    fn rearm_carries_imbos_note() {
        let r = rearm();
        assert!(r.starts_with(&note()));
        assert!(r.contains(crate::mcp::INSTRUCTIONS));
        assert!(r.ends_with(&hook_command(&home(), "wait")));
    }

    #[test]
    fn mode_is_set_and_put_back() {
        let text = "# mine\ndefault_model = \"x\"\n[mcp]\ntool_timeout_ms = 5\n";
        assert_eq!(mode(text), "manual");
        let yolo = with_mode(text, Some("yolo"));
        assert_eq!(mode(&yolo), "yolo");
        assert!(yolo.starts_with("default_permission_mode = \"yolo\"\n"));
        assert_eq!(with_mode(&yolo, None), text);
        let auto = "default_permission_mode = \"auto\"\n";
        assert_eq!(mode(auto), "auto");
        assert_eq!(
            with_mode(auto, Some("yolo")),
            "default_permission_mode = \"yolo\"\n"
        );
        assert_eq!(mode("default_yolo = true\n"), "yolo");
    }
}
