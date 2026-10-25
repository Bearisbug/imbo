//! ZCode: the agent runs `imbo wait` in the background and it waits for
//! the session's messages; the stop hook asks for it at the end of each
//! turn (FLOW-LISTEN). ZCode keeps no record of which sessions are open, so
//! a session counts as online while it listens or shortly after it called.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use base64::Engine;
use futures::future::BoxFuture;
use imbo_proto::api::MessageView;
use imbo_proto::local::LocalFile;
use serde_json::{Map, Value, json};

use super::{
    Adapter, Connected, Found, Install, Paste, PastedImage, Registration, drop_empty, edit_json,
    env, hook_command, object, open_ro, our_hook, server_env,
};
use crate::paths::{Home, exe, tilde, user_home};

/// A session seen only through its calls and hooks stays online this long.
const CALLED_TTL: Duration = Duration::from_secs(5 * 60);
/// Longest context the prompt hook adds; ZCode's own limit isn't known, the
/// other tools take 10000 characters.
const CONTEXT_LIMIT: usize = 8000;
/// The hooks `imbo setup` installs.
const EVENTS: [&str; 2] = ["Stop", "UserPromptSubmit"];

pub static ZCODE: Zcode = Zcode {
    called: Mutex::new(BTreeMap::new()),
};

pub struct Zcode {
    /// Sessions that called Imbo or ran its hooks, and when they last did.
    called: Mutex<BTreeMap<String, Instant>>,
}

impl Zcode {
    fn touch(&self, session_id: &str) -> Found {
        self.called
            .lock()
            .unwrap()
            .insert(session_id.into(), Instant::now());
        Found {
            session_id: session_id.into(),
            title: open_ro(&db_path())
                .and_then(|db| title(&db, session_id))
                .unwrap_or_default(),
            online: true,
        }
    }
}

impl Adapter for Zcode {
    fn id(&self) -> &'static str {
        imbo_proto::TOOL_ZCODE
    }

    fn name(&self) -> &'static str {
        "ZCode"
    }

    /// Each call's `_meta` names the session; commands the agent runs get
    /// it from the file their output goes to. A subagent's session counts
    /// as its main session.
    fn session(&self, meta: &[&Map<String, Value>]) -> Option<String> {
        let id = meta
            .iter()
            .find_map(|m| meta_session(m))
            .or_else(|| session_in(&crate::procs::stdout_path()?))?;
        Some(main_session(&id))
    }

    fn mcp_client(&self) -> Option<&'static str> {
        Some("zcode")
    }

    fn owns_mcp(&self, client: &str) -> bool {
        client.starts_with("zcode") || self.marked()
    }

    fn marked(&self) -> bool {
        env("ZCODE_RUNTIME_ENV").is_some()
    }

    fn env_pass(&self) -> &'static [&'static str] {
        &[]
    }

    fn env_session(&self) -> &'static [&'static str] {
        &["ZCODE_SESSION_ID", "ZCODE_RUNTIME_ENV"]
    }

    fn announce_call(&self, session_id: &str) -> Option<Registration> {
        Some(Registration {
            session_id: session_id.into(),
            messaging_socket: None,
            messaging_token: None,
            pid: unsafe { libc::getppid() } as u32,
        })
    }

    fn register(&self, r: Registration) -> Option<Found> {
        Some(self.touch(&r.session_id))
    }

    fn adopt(&self, session_id: &str) -> Option<Found> {
        Some(self.touch(session_id))
    }

    fn poll_every(&self) -> Duration {
        Duration::from_secs(5)
    }

    /// Sessions with a listener waiting, and those that called within
    /// `CALLED_TTL`; deleted ones are left out.
    fn discover(&self) -> BoxFuture<'_, Result<Vec<Found>, String>> {
        let mut ids: BTreeSet<String> = crate::daemon::listen::sessions(self.id())
            .into_iter()
            .collect();
        {
            let mut called = self.called.lock().unwrap();
            called.retain(|_, t| t.elapsed() < CALLED_TTL);
            ids.extend(called.keys().cloned());
        }
        let db = open_ro(&db_path());
        let list = ids
            .into_iter()
            .filter(|id| !db.as_ref().is_some_and(|db| deleted(db, id)))
            .map(|id| Found {
                title: db
                    .as_ref()
                    .and_then(|db| title(db, &id))
                    .unwrap_or_default(),
                session_id: id,
                online: true,
            })
            .collect();
        Box::pin(std::future::ready(Ok(list)))
    }

    fn channel(&self, session_id: &str) -> &'static str {
        super::listen_channel(self.id(), session_id)
    }

    fn listens(&self) -> bool {
        true
    }

    /// ZCode writes a background command's output to
    /// `…/zcode-bash-XXXXXX/<session>/<call>-stdout.log`. Run by a subagent
    /// or a workflow, the folder names that one, which can't be woken.
    fn wait_target(&self) -> Option<(String, Option<u32>)> {
        let id = session_in(&crate::procs::stdout_path()?)?;
        if sub_session(&id) {
            return None;
        }
        Some((id, super::tool_process()))
    }

    fn hook<'a>(&'a self, event: &'a str, input: &'a Value) -> BoxFuture<'a, Option<String>> {
        Box::pin(async move {
            let session_id =
                text(input, &["sessionId", "session_id"]).or_else(|| env("ZCODE_SESSION_ID"))?;
            let pid = unsafe { libc::getppid() } as u32;
            super::hook_register(self.id(), &session_id, pid).await;
            match event {
                // Asked once per turn: a second stop of the same turn ends it.
                "Stop" => {
                    let again = ["stopHookActive", "stop_hook_active"]
                        .iter()
                        .any(|k| input[*k].as_bool() == Some(true));
                    if again || !super::needs_listener(self.id(), &session_id).await {
                        return None;
                    }
                    Some(json!({ "decision": "block", "reason": rearm() }).to_string())
                }
                "UserPromptSubmit" => {
                    let ctx = super::hook_context(self.id(), &session_id, CONTEXT_LIMIT).await?;
                    let out = json!({ "hookSpecificOutput": {
                        "hookEventName": "UserPromptSubmit", "additionalContext": ctx } });
                    Some(out.to_string())
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
            "ZCode sessions get messages through their listener".into(),
        )))
    }

    fn paste(&self) -> Paste {
        Paste::Numbered(
            "用户在会话里附上的图片，把它在用户最近一条带图消息里的序号（从 1 数）写进 images，作为附件发出。",
        )
    }

    fn pasted_image(&self, session_id: &str, n: u32) -> Result<PastedImage, String> {
        stored_image(session_id, n).ok_or_else(|| {
            format!(
                "会话记录里找不到用户最近一条带图消息里的第 {n} 张图，请让使用者重新附上后再发。"
            )
        })
    }

    fn find(&self) -> Install {
        [
            PathBuf::from("/Applications/ZCode.app"),
            user_home().join("Applications/ZCode.app"),
        ]
        .into_iter()
        .find(|p| p.is_dir())
        .map_or(Install::Missing, Install::Found)
    }

    fn changes(&self) -> String {
        format!(
            "ZCode  {}  mcp.servers.imbo; Stop and UserPromptSubmit hooks, hooks.enabled on; Imbo's tools in permission.allowedTools",
            tilde(&config_file())
        )
    }

    /// The desktop app reads its MCP servers when it starts, so ZCode has
    /// to be restarted. A server the user turned off stays off.
    fn connect(&self, home: &Home, _bin: &Path) -> Result<Connected, String> {
        let server = json!({
            "type": "stdio",
            "command": exe().to_string_lossy(),
            "args": ["mcp"],
            "env": server_env(home, self.id()),
            "enabled": true,
        });
        let by_hand = format!(
            "\"mcp\": {{\"servers\": {{\"imbo\": {server}}}}} and the Stop and UserPromptSubmit hooks `imbo hook zcode <event>`"
        );
        let mut off = false;
        edit_json(&config_file(), &by_hand, |root| {
            object(root, "features")?
                .entry("mcp")
                .or_insert(json!(true));
            let servers = object(object(root, "mcp")?, "servers")?;
            let mut server = server.clone();
            if servers.get("imbo").and_then(|s| s.get("enabled")) == Some(&json!(false)) {
                server["enabled"] = json!(false);
                off = true;
            }
            servers.insert("imbo".into(), server);
            let hooks = object(root, "hooks")?;
            hooks.insert("enabled".into(), json!(true));
            let events = object(hooks, "events")?;
            for event in EVENTS {
                let command = hook_command(home, &format!("hook zcode {event}"));
                set_hook(events, event, home, Some(&command))?;
            }
            let allowed = object(root, "permission")?
                .entry("allowedTools")
                .or_insert_with(|| json!([]))
                .as_array_mut()
                .ok_or("`permission.allowedTools` isn't a list")?;
            for name in allowed_names() {
                if !allowed.contains(&name) {
                    allowed.push(name);
                }
            }
            Ok(())
        })?;
        restrict(&config_file());
        let warning = off.then(|| {
            "Imbo is turned off in ZCode's MCP settings; turn it on there to use it".to_string()
        });
        Ok(Connected {
            detail: "MCP server imbo + stop and prompt hooks; restart ZCode to load them",
            warning,
        })
    }

    fn disconnect(&self, home: &Home, _bin: Option<&Path>) {
        let _ = edit_json(&config_file(), "", |root| {
            if let Some(Value::Object(mcp)) = root.get_mut("mcp") {
                if let Some(Value::Object(servers)) = mcp.get_mut("servers") {
                    servers.remove("imbo");
                }
                drop_empty(mcp, "servers");
            }
            drop_empty(root, "mcp");
            if let Some(Value::Object(hooks)) = root.get_mut("hooks")
                && let Some(Value::Object(events)) = hooks.get_mut("events")
            {
                for event in EVENTS {
                    set_hook(events, event, home, None)?;
                }
                drop_empty(hooks, "events");
            }
            if let Some(Value::Object(p)) = root.get_mut("permission") {
                if let Some(Value::Array(allowed)) = p.get_mut("allowedTools") {
                    let ours = allowed_names();
                    allowed.retain(|t| !ours.contains(t));
                }
                drop_empty(p, "allowedTools");
            }
            drop_empty(root, "permission");
            Ok(())
        });
    }
}

fn config_file() -> PathBuf {
    user_home().join(".zcode/cli/config.json")
}

fn db_path() -> PathBuf {
    user_home().join(".zcode/cli/db/db.sqlite")
}

/// ZCode keeps its config readable by the user only; so does Imbo.
fn restrict(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
}

/// ZCode names MCP tools `mcp__<server>__<tool>`.
fn allowed_names() -> Vec<Value> {
    crate::mcp::tool_names()
        .into_iter()
        .map(|t| json!(format!("mcp__imbo__{t}")))
        .collect()
}

/// Put Imbo's hook for `event` in `events`, replacing the one an earlier
/// setup wrote; `None` only takes it out.
fn set_hook(
    events: &mut Map<String, Value>,
    event: &str,
    home: &Home,
    command: Option<&str>,
) -> Result<(), String> {
    let groups = events
        .entry(event)
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .ok_or_else(|| format!("`hooks.events.{event}` isn't a list"))?;
    for g in groups.iter_mut() {
        if let Some(Value::Array(hooks)) = g.get_mut("hooks") {
            hooks.retain(|h| {
                !h["command"]
                    .as_str()
                    .is_some_and(|c| our_hook(c, home, imbo_proto::TOOL_ZCODE))
            });
        }
    }
    groups.retain(|g| g["hooks"].as_array().is_none_or(|h| !h.is_empty()));
    if let Some(command) = command {
        groups
            .push(json!({ "hooks": [ { "type": "command", "command": command, "timeout": 15 } ] }));
    }
    drop_empty(events, event);
    Ok(())
}

/// The stop hook's request when no listener waits (API-LOCAL-011).
fn rearm() -> String {
    format!(
        "Imbo 收件监听没有在运行。请用 Bash 工具在后台运行下面这条命令（run_in_background=true，description 写「{}」），不要等它结束，启动后直接结束本轮，不用再说别的：{}",
        super::WAKE,
        hook_command(&Home::resolve(), "wait")
    )
}

/// The first of `keys` that `input` has as text; ZCode writes both casings.
fn text(input: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|k| input[*k].as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// ZCode's request context in a call's `_meta`: at the top level, and
/// again under `com.zcode/request-context`.
fn meta_session(m: &Map<String, Value>) -> Option<String> {
    let ctx = match m.get("com.zcode/request-context") {
        Some(Value::Object(c)) => c,
        _ if m.contains_key("runtime_scope") => m,
        _ => return None,
    };
    ctx.get("session_id")?
        .as_str()
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

/// The session folder in the path of a background command's output.
fn session_in(path: &Path) -> Option<String> {
    let parts: Vec<&str> = path.iter().filter_map(|c| c.to_str()).collect();
    let i = parts.iter().position(|c| c.starts_with("zcode-bash-"))?;
    (parts.len() == i + 3).then(|| parts[i + 1].to_string())
}

fn sub_session(id: &str) -> bool {
    id.starts_with("sess_subagent_") || id.starts_with("sess_workflow_")
}

/// The main session a subagent's session belongs to.
fn main_session(id: &str) -> String {
    if !sub_session(id) {
        return id.into();
    }
    let Some(db) = open_ro(&db_path()) else {
        return id.into();
    };
    let mut id = id.to_string();
    while sub_session(&id) {
        let parent = db.query_row("SELECT parent_id FROM session WHERE id = ?1", [&id], |r| {
            r.get::<_, Option<String>>(0)
        });
        match parent {
            Ok(Some(p)) if p != id => id = p,
            _ => break,
        }
    }
    id
}

fn title(db: &rusqlite::Connection, session_id: &str) -> Option<String> {
    db.query_row(
        "SELECT title FROM session WHERE id = ?1",
        [session_id],
        |r| r.get::<_, String>(0),
    )
    .ok()
}

fn deleted(db: &rusqlite::Connection, session_id: &str) -> bool {
    db.query_row(
        "SELECT time_deleted IS NOT NULL FROM session WHERE id = ?1",
        [session_id],
        |r| r.get::<_, bool>(0),
    )
    .unwrap_or(false)
}

/// Picture `n` of the latest user message with pictures. ZCode stores each
/// as a data URL in an artifact file named after the part's
/// `zcode-artifact://<session>/<artifact>` URL.
fn stored_image(session_id: &str, n: u32) -> Option<PastedImage> {
    let db = open_ro(&db_path())?;
    let mut st = db
        .prepare(
            "SELECT p.message_id, json_extract(p.data, '$.url') FROM part p
             JOIN message m ON m.id = p.message_id
             WHERE p.session_id = ?1 AND json_extract(m.data, '$.role') = 'user'
               AND json_extract(p.data, '$.type') = 'file'
             ORDER BY m.time_created DESC, p.sequence",
        )
        .ok()?;
    let rows: Vec<(String, Option<String>)> = st
        .query_map([session_id], |r| Ok((r.get(0)?, r.get(1)?)))
        .ok()?
        .filter_map(Result::ok)
        .collect();
    let latest = &rows.first()?.0;
    let url = rows
        .iter()
        .filter(|(m, _)| m == latest)
        .nth(n.checked_sub(1)? as usize)?
        .1
        .clone()?;
    let artifact = url.rsplit('/').next()?;
    let dir = user_home().join(".zcode/cli/artifacts").join(session_id);
    let file = std::fs::read_dir(dir)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .find(|p| {
            p.file_name()
                .and_then(|f| f.to_str())
                .is_some_and(|f| f.ends_with(&format!("{artifact}.txt")))
        })?;
    data_url(&std::fs::read_to_string(file).ok()?)
}

fn data_url(text: &str) -> Option<PastedImage> {
    let (head, data) = text.trim().strip_prefix("data:")?.split_once(',')?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data.trim())
        .ok()?;
    Some(PastedImage {
        media_type: head.strip_suffix(";base64")?.to_string(),
        bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_session_in_meta_and_in_the_output_path() {
        let top = json!({ "session_id": "sess_a", "runtime_scope": "main" });
        let nested =
            json!({ "progressToken": 1, "com.zcode/request-context": { "session_id": "sess_b" } });
        let other = json!({ "session_id": "sess_c" });
        assert_eq!(
            meta_session(top.as_object().unwrap()).as_deref(),
            Some("sess_a")
        );
        assert_eq!(
            meta_session(nested.as_object().unwrap()).as_deref(),
            Some("sess_b")
        );
        assert_eq!(meta_session(other.as_object().unwrap()), None);

        let out = Path::new("/var/folders/x/T/zcode-bash-Ab12/sess_9/call_1-stdout.log");
        assert_eq!(session_in(out).as_deref(), Some("sess_9"));
        assert_eq!(
            session_in(Path::new("/var/folders/x/T/zcode-bash-Ab12/sess_9")),
            None
        );
        assert_eq!(session_in(Path::new("/dev/ttys003")), None);
        assert!(sub_session("sess_subagent_agent_1") && !sub_session("sess_9"));
    }

    #[test]
    fn hooks_are_replaced_and_taken_out_leaving_others() {
        let home = Home {
            dir: user_home().join(".imbo"),
        };
        let mut events = json!({
            "Stop": [
                { "hooks": [ { "type": "command", "command": "/old/imbo hook zcode Stop" } ] },
                { "hooks": [ { "type": "command", "command": "my-stop.sh" } ] }
            ]
        });
        let events = events.as_object_mut().unwrap();
        set_hook(events, "Stop", &home, Some("/new/imbo hook zcode Stop")).unwrap();
        let commands: Vec<_> = events["Stop"]
            .as_array()
            .unwrap()
            .iter()
            .map(|g| g["hooks"][0]["command"].as_str().unwrap())
            .collect();
        assert_eq!(commands, ["my-stop.sh", "/new/imbo hook zcode Stop"]);
        set_hook(events, "Stop", &home, None).unwrap();
        set_hook(events, "UserPromptSubmit", &home, None).unwrap();
        assert_eq!(
            Value::Object(events.clone()),
            json!({ "Stop": [ { "hooks": [ { "type": "command", "command": "my-stop.sh" } ] } ] })
        );
    }

    #[test]
    fn reads_data_urls() {
        let img = data_url("data:image/png;base64,aGVsbG8=\n").unwrap();
        assert_eq!(
            (img.media_type.as_str(), img.bytes.as_slice()),
            ("image/png", &b"hello"[..])
        );
        assert!(data_url("hello").is_none());
    }
}
