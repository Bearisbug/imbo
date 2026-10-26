//! Cursor CLI (`cursor-agent`): the stop hook itself waits for the
//! session's messages and, when they come, starts a turn with a notice
//! (FLOW-LISTEN). While a turn runs, messages come in after each tool call;
//! when the user writes, before the prompt (API-LOCAL-003).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use futures::future::BoxFuture;
use imbo_proto::api::MessageView;
use imbo_proto::local::{LocalFile, LocalRequest};
use serde_json::{Map, Value, json};

use super::{
    Adapter, Connected, Found, Install, Paste, PastedImage, Registration, drop_empty, edit_json,
    env, hook_command, object, our_hook, remember, remembered_all, server_env,
};
use crate::local::{Heard, Local};
use crate::paths::{Home, exe, tilde, user_home};

/// Cursor drops hook context over 10000 characters, counted together with
/// other hooks' context.
const CONTEXT_LIMIT: usize = 8000;
/// Cursor gives up on a tool call after 60 seconds (API-MCP-001).
const CALL_LIMIT: Duration = Duration::from_secs(60);
/// Seconds Cursor lets the stop hook run; a larger number overflows its
/// timer and ends the hook at once.
const STOP_TIMEOUT_S: u64 = 2_000_000;
/// Lets Imbo's tools run without asking.
const RULE: &str = "Mcp(imbo:*)";

pub static CURSOR: Cursor = Cursor;

pub struct Cursor;

impl Adapter for Cursor {
    fn id(&self) -> &'static str {
        imbo_proto::TOOL_CURSOR
    }

    fn name(&self) -> &'static str {
        "Cursor"
    }

    /// The pre-call hook puts the session into each call; shell commands
    /// get `CURSOR_CONVERSATION_ID`.
    fn session(&self, meta: &[&Map<String, Value>]) -> Option<String> {
        meta.iter()
            .find_map(|m| m.get("imbo_session")?.as_str().map(str::to_string))
            .or_else(|| env("CURSOR_CONVERSATION_ID"))
    }

    fn mcp_client(&self) -> Option<&'static str> {
        Some("Cursor")
    }

    /// Cursor calls MCP tools by their bare names, so plain names would
    /// clash with other servers' tools.
    fn tool_prefix(&self) -> &'static str {
        "imbo_"
    }

    fn call_limit(&self) -> Option<Duration> {
        Some(CALL_LIMIT)
    }

    fn marked(&self) -> bool {
        env("CURSOR_AGENT").as_deref() == Some("1")
    }

    fn env_pass(&self) -> &'static [&'static str] {
        &[]
    }

    fn env_session(&self) -> &'static [&'static str] {
        &[
            "CURSOR_CONVERSATION_ID",
            "CURSOR_TRANSCRIPT_PATH",
            "CURSOR_AGENT",
        ]
    }

    /// The MCP process is a child of the Cursor process.
    fn announce_call(&self, session_id: &str) -> Option<Registration> {
        Some(Registration {
            session_id: session_id.into(),
            messaging_socket: None,
            messaging_token: None,
            pid: unsafe { libc::getppid() } as u32,
        })
    }

    /// One conversation is open per Cursor process; registering another
    /// one, as after `/clear`, replaces it.
    fn register(&self, r: Registration) -> Option<Found> {
        remember(self.id(), r.pid, &r.session_id);
        Some(found(&r.session_id, crate::procs::alive(r.pid)))
    }

    fn adopt(&self, session_id: &str) -> Option<Found> {
        remembered_all(self.id())
            .into_iter()
            .any(|(_, s)| s == session_id)
            .then(|| found(session_id, true))
    }

    fn poll_every(&self) -> Duration {
        Duration::from_secs(5)
    }

    /// The conversation each running Cursor process has open, as its hooks
    /// last reported it.
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

    /// A user who writes while the stop hook waits starts a turn, and that
    /// turn's stop hook is the one to keep.
    fn keep_newest(&self) -> bool {
        true
    }

    fn hook<'a>(&'a self, event: &'a str, input: &'a Value) -> BoxFuture<'a, Option<String>> {
        Box::pin(async move {
            // The editor reads the same hooks file; only the command line
            // works with Imbo (ADR-018). Its stop hook must end at once.
            let ours = env("CURSOR_INVOKED_AS").is_some();
            let session = input["conversation_id"].as_str().filter(|s| !s.is_empty());
            let (true, Some(session_id), Some(pid)) = (ours, session, super::tool_process()) else {
                return (event == "stop").then(|| "{}".to_string());
            };
            match event {
                "stop" => self.stop(input, session_id, pid).await,
                "preToolUse" => {
                    // After `/clear` the tool hooks still carry the old
                    // conversation; the one the process has open counts.
                    let open = open_chats(pid);
                    let session_id = match open.as_slice() {
                        [only] if !open.iter().any(|c| c == session_id) => only.clone(),
                        _ => session_id.to_string(),
                    };
                    super::hook_register(self.id(), &session_id, pid).await;
                    let mut args = input["tool_input"].as_object().cloned().unwrap_or_default();
                    args.insert("imbo_session".into(), json!(session_id));
                    Some(json!({ "updated_input": args }).to_string())
                }
                "postToolUse" | "beforeSubmitPrompt" => {
                    if event == "beforeSubmitPrompt" {
                        super::hook_register(self.id(), session_id, pid).await;
                    }
                    let ctx = super::hook_context(self.id(), session_id, CONTEXT_LIMIT).await?;
                    Some(json!({ "additional_context": ctx }).to_string())
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
            "Cursor sessions get messages through their stop hook".into(),
        )))
    }

    fn paste(&self) -> Paste {
        Paste::Path(
            " Images the user pasted in this session: <image_files> in the message lists where they were saved; put those paths in files to send them as attachments.",
        )
    }

    fn pasted_image(&self, _session_id: &str, _n: u32) -> Result<PastedImage, String> {
        Err("<image_files> in the message lists where pasted images were saved; put those paths in files.".into())
    }

    /// The install script links `cursor-agent` and `agent` in
    /// `~/.local/bin`; `agent` alone could be another program.
    fn find(&self) -> Install {
        let own = user_home().join(".local/bin/cursor-agent");
        match crate::paths::which("cursor-agent").or_else(|| own.is_file().then_some(own)) {
            Some(p) => Install::Found(p),
            None => Install::Missing,
        }
    }

    fn changes(&self) -> String {
        format!(
            "Cursor  {}  mcpServers.imbo; {}  stop, preToolUse, postToolUse, beforeSubmitPrompt hooks; {}  {RULE} in permissions.allow",
            tilde(&mcp_file()),
            tilde(&hooks_file()),
            tilde(&cli_config())
        )
    }

    /// The command line reads these files when it starts.
    fn connect(&self, home: &Home, _bin: &Path) -> Result<Connected, String> {
        let server = json!({
            "command": exe().to_string_lossy(),
            "args": ["mcp"],
            "env": server_env(home, self.id()),
        });
        edit_json(
            &mcp_file(),
            &format!("\"mcpServers\": {{\"imbo\": {server}}}"),
            |root| {
                object(root, "mcpServers")?.insert("imbo".into(), server.clone());
                Ok(())
            },
        )?;
        edit_json(
            &hooks_file(),
            "the stop, preToolUse, postToolUse and beforeSubmitPrompt hooks `imbo hook cursor <event>`",
            |root| set_hooks(root, home, true),
        )?;
        let warning = edit_json(
            &cli_config(),
            &format!("\"{RULE}\" in permissions.allow, or Cursor asks before each Imbo tool"),
            |root| allow(root, true),
        )
        .err();
        Ok(Connected {
            detail: "MCP server imbo + hooks; restart running agent sessions to load them",
            warning,
        })
    }

    fn disconnect(&self, home: &Home, _bin: Option<&Path>) {
        let _ = edit_json(&mcp_file(), "", |root| {
            if let Some(Value::Object(servers)) = root.get_mut("mcpServers") {
                servers.remove("imbo");
            }
            Ok(())
        });
        let _ = edit_json(&hooks_file(), "", |root| set_hooks(root, home, false));
        let _ = edit_json(&cli_config(), "", |root| allow(root, false));
    }
}

impl Cursor {
    /// Wait for the conversation's messages and start a turn with a notice
    /// (API-LOCAL-010). What a stopped or failed turn's stop hook prints is
    /// thrown away, so its listener is taken back instead.
    async fn stop(&self, input: &Value, session_id: &str, pid: u32) -> Option<String> {
        let local = Local::new();
        if input["status"].as_str() != Some("completed") {
            let req = LocalRequest::Unlisten {
                tool: self.id().into(),
                session_id: session_id.into(),
            };
            let _ = local.send_with(&req, Duration::from_secs(1)).await;
            return Some("{}".into());
        }
        super::hook_register(self.id(), session_id, pid).await;
        let show = |text: &str| {
            // After `/clear` the process shows another conversation.
            if !open_chats(pid).iter().any(|c| c == session_id) {
                return false;
            }
            let mut stdout = std::io::stdout();
            let out = json!({ "followup_message": text });
            writeln!(stdout, "{out}")
                .and_then(|_| stdout.flush())
                .is_ok()
        };
        match local.listen(self.id(), session_id, Some(pid), show).await {
            Ok(Heard::Messages) => None,
            _ => Some("{}".into()),
        }
    }
}

fn found(session_id: &str, online: bool) -> Found {
    Found {
        session_id: session_id.into(),
        title: title(session_id).unwrap_or_default(),
        online,
    }
}

/// Cursor names a conversation about two seconds after its first prompt,
/// in `~/.cursor/chats/<workspace>/<conversation>/meta.json`.
fn title(session_id: &str) -> Option<String> {
    std::fs::read_dir(user_home().join(".cursor/chats"))
        .ok()?
        .filter_map(|e| e.ok())
        .find_map(|e| {
            let meta = std::fs::read(e.path().join(session_id).join("meta.json")).ok()?;
            let v: Value = serde_json::from_slice(&meta).ok()?;
            v["title"].as_str().map(str::to_string)
        })
}

/// Conversations whose `store.db` the Cursor process `pid` has open.
fn open_chats(pid: u32) -> Vec<String> {
    let mut chats: Vec<String> = crate::procs::open_files(pid)
        .into_iter()
        .filter(|p| {
            p.file_name()
                .and_then(|f| f.to_str())
                .is_some_and(|f| f.starts_with("store.db"))
                && p.to_string_lossy().contains("/.cursor/chats/")
        })
        .filter_map(|p| Some(p.parent()?.file_name()?.to_str()?.to_string()))
        .collect();
    chats.sort();
    chats.dedup();
    chats
}

/// Cursor reads these two from the home folder whatever its settings say.
fn mcp_file() -> PathBuf {
    user_home().join(".cursor/mcp.json")
}

fn hooks_file() -> PathBuf {
    user_home().join(".cursor/hooks.json")
}

/// The command line's settings, in `CURSOR_CONFIG_DIR`, else
/// `$XDG_CONFIG_HOME/cursor`, else `~/.cursor`.
fn cli_config() -> PathBuf {
    let dir = env("CURSOR_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| env("XDG_CONFIG_HOME").map(|x| Path::new(&x).join("cursor")))
        .unwrap_or_else(|| user_home().join(".cursor"));
    dir.join("cli-config.json")
}

/// Put Imbo's hooks into `hooks.json`, replacing those an earlier setup
/// wrote, or only take them out.
fn set_hooks(root: &mut Map<String, Value>, home: &Home, add: bool) -> Result<(), String> {
    if add {
        root.entry("version").or_insert(json!(1));
    }
    let hooks = object(root, "hooks")?;
    let events = [
        (
            "stop",
            json!({ "timeout": STOP_TIMEOUT_S, "loop_limit": null }),
        ),
        ("preToolUse", json!({ "matcher": "^MCP:imbo_" })),
        ("postToolUse", json!({})),
        ("beforeSubmitPrompt", json!({})),
    ];
    for (event, extra) in events {
        let list = hooks
            .entry(event)
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .ok_or_else(|| format!("`hooks.{event}` isn't a list"))?;
        list.retain(|h| {
            !h["command"]
                .as_str()
                .is_some_and(|c| our_hook(c, home, imbo_proto::TOOL_CURSOR))
        });
        if add {
            let mut entry =
                json!({ "command": hook_command(home, &format!("hook cursor {event}")) });
            if let (Some(e), Value::Object(extra)) = (entry.as_object_mut(), extra) {
                e.extend(extra);
            }
            list.push(entry);
        }
        drop_empty(hooks, event);
    }
    drop_empty(root, "hooks");
    Ok(())
}

fn allow(root: &mut Map<String, Value>, add: bool) -> Result<(), String> {
    if !add && !root.contains_key("permissions") {
        return Ok(());
    }
    let rules = object(root, "permissions")?
        .entry("allow")
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .ok_or("`permissions.allow` isn't a list")?;
    let has = rules.iter().any(|r| r == RULE);
    if add && !has {
        rules.push(json!(RULE));
    } else if !add {
        rules.retain(|r| r != RULE);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hooks_are_added_once_and_taken_out_leaving_others() {
        let home = Home {
            dir: user_home().join(".imbo"),
        };
        let mut v = json!({
            "version": 1,
            "hooks": {
                "stop": [ { "command": "/old/imbo hook cursor stop" }, { "command": "notify.sh" } ],
                "afterFileEdit": [ { "command": "fmt.sh" } ]
            }
        });
        let root = v.as_object_mut().unwrap();
        set_hooks(root, &home, true).unwrap();
        set_hooks(root, &home, true).unwrap();
        let stop = root["hooks"]["stop"].as_array().unwrap();
        assert_eq!(stop.len(), 2);
        assert_eq!(stop[0]["command"], "notify.sh");
        assert!(
            stop[1]["command"]
                .as_str()
                .unwrap()
                .ends_with(" hook cursor stop")
        );
        assert_eq!(stop[1]["timeout"], 2_000_000);
        assert_eq!(stop[1]["loop_limit"], Value::Null);
        assert_eq!(root["hooks"]["preToolUse"][0]["matcher"], "^MCP:imbo_");

        set_hooks(root, &home, false).unwrap();
        assert_eq!(
            v,
            json!({ "version": 1, "hooks": {
                "stop": [ { "command": "notify.sh" } ],
                "afterFileEdit": [ { "command": "fmt.sh" } ] } })
        );
    }

    #[test]
    fn allow_rule_is_added_once_and_removed() {
        let mut v = json!({ "permissions": { "allow": ["Shell(ls)"], "deny": [] }, "model": "x" });
        let root = v.as_object_mut().unwrap();
        allow(root, true).unwrap();
        allow(root, true).unwrap();
        assert_eq!(root["permissions"]["allow"], json!(["Shell(ls)", RULE]));
        allow(root, false).unwrap();
        assert_eq!(root["permissions"]["allow"], json!(["Shell(ls)"]));
        let mut empty = Map::new();
        allow(&mut empty, false).unwrap();
        assert!(empty.is_empty());
    }
}
