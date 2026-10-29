//! MiniMax Code: the agent runs `imbo wait` in the background and it waits
//! for the session's messages; the stop hook asks for it at the end of each
//! turn (FLOW-LISTEN). A plugin brings the MCP server and the hooks, and
//! the hooks tell the daemon which session each MiniMax Code process has
//! open.

use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::time::Duration;

use futures::future::BoxFuture;
use imbo_proto::api::MessageView;
use imbo_proto::local::LocalFile;
use serde_json::{Map, Value, json};

use super::{
    Adapter, Connected, Found, Install, Paste, PastedImage, Registration, env, hook_command,
    on_path, open_ro, remember, remembered, remembered_all, server_env, write_json,
};
use crate::paths::{Home, exe, tilde, user_home};

/// MiniMax Code closes an MCP connection whose call started 15 minutes
/// ago, so calls get a little less (API-MCP-001).
const CALL_TIMEOUT_MS: u64 = 840_000;
/// Seconds the background `imbo wait` may run: without a timeout MiniMax
/// Code ends background commands after an hour.
const WAIT_TIMEOUT_S: u64 = 2_000_000;
/// MiniMax Code cuts hook context longer than 10000 characters.
const CONTEXT_LIMIT: usize = 9000;
/// Every tool of the plugin's MCP server, as hooks see their names.
const TOOL_MATCHER: &str = "mcp__plugin_imbo_imbo__.*";

pub static MCODE: Mcode = Mcode;

pub struct Mcode;

impl Adapter for Mcode {
    fn id(&self) -> &'static str {
        imbo_proto::TOOL_MCODE
    }

    fn name(&self) -> &'static str {
        "MiniMax Code"
    }

    /// The pre-call hook puts the session into each call. Commands the
    /// agent runs are children of the MiniMax Code process, whose session
    /// its hooks reported.
    fn session(&self, meta: &[&Map<String, Value>]) -> Option<String> {
        meta.iter()
            .find_map(|m| m.get("imbo_session")?.as_str().map(str::to_string))
            .or_else(|| {
                self.marked()
                    .then(|| super::ancestor_session(self.id()).map(|(_, s)| s))?
            })
    }

    fn mcp_client(&self) -> Option<&'static str> {
        Some("mavis-local-runtime-mcp")
    }

    fn marked(&self) -> bool {
        env("__MAVIS_MCODE_TOOLS_BROKER_ENDPOINT").is_some()
    }

    fn env_pass(&self) -> &'static [&'static str] {
        &[]
    }

    fn env_session(&self) -> &'static [&'static str] {
        &["__MAVIS_MCODE_TOOLS_BROKER_ENDPOINT"]
    }

    /// The MCP process is a child of the MiniMax Code process.
    fn announce_call(&self, session_id: &str) -> Option<Registration> {
        Some(Registration {
            session_id: session_id.into(),
            messaging_socket: None,
            messaging_token: None,
            pid: unsafe { libc::getppid() } as u32,
            config_dir: None,
        })
    }

    fn register(&self, r: Registration) -> Option<Found> {
        remember(self.id(), r.pid, &r.session_id);
        Some(found(&r.session_id, running(&leases(), r.pid)))
    }

    fn adopt(&self, session_id: &str) -> Option<Found> {
        let (pid, _) = remembered_all(self.id())
            .into_iter()
            .find(|(_, s)| s == session_id)?;
        Some(found(session_id, running(&leases(), pid)))
    }

    fn poll_every(&self) -> Duration {
        Duration::from_secs(5)
    }

    /// The session each running MiniMax Code process has open.
    fn discover(&self) -> BoxFuture<'_, Result<Vec<Found>, String>> {
        let leases = leases();
        let db = open_ro(&data_dir().join("v2/sqlite/runtime-state.sqlite"));
        let list = remembered_all(self.id())
            .into_iter()
            .filter(|(pid, _)| running(&leases, *pid))
            .map(|(_, id)| Found {
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

    /// The MiniMax Code process above `imbo wait`, and the session its
    /// hooks last reported.
    fn wait_target(&self) -> Option<(String, Option<u32>)> {
        let pid = own_process()?;
        Some((remembered(self.id(), pid)?, Some(pid)))
    }

    fn hook<'a>(&'a self, event: &'a str, input: &'a Value) -> BoxFuture<'a, Option<String>> {
        Box::pin(async move {
            let session_id = input["session_id"].as_str().filter(|s| !s.is_empty())?;
            let pid = own_process().unwrap_or_else(|| unsafe { libc::getppid() } as u32);
            if event == "SessionEnd" {
                // `/new` or switching sessions: the session goes offline.
                // Its listener stays, for when the user switches back; one
                // that ended would start a turn in the session left.
                if matches!(input["reason"].as_str(), Some("clear" | "resume_other")) {
                    super::forget(self.id(), pid, session_id);
                }
                return None;
            }
            super::hook_register(self.id(), session_id, pid).await;
            match event {
                // MiniMax Code takes the input as given here, not merged.
                "PreToolUse" => {
                    let mut args = input["tool_input"].as_object().cloned().unwrap_or_default();
                    args.insert("imbo_session".into(), json!(session_id));
                    let out = json!({ "hookSpecificOutput": {
                        "hookEventName": "PreToolUse", "updatedInput": args } });
                    Some(out.to_string())
                }
                "UserPromptSubmit" => {
                    let ctx = super::hook_context(self.id(), session_id, CONTEXT_LIMIT).await?;
                    let out = json!({ "hookSpecificOutput": {
                        "hookEventName": "UserPromptSubmit", "additionalContext": ctx } });
                    Some(out.to_string())
                }
                // Asked once per turn: a second stop of the same turn ends it.
                "Stop" => {
                    if input["stop_hook_active"].as_bool() == Some(true)
                        || !super::needs_listener(self.id(), session_id).await
                    {
                        return None;
                    }
                    Some(json!({ "decision": "block", "reason": rearm() }).to_string())
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
            "MiniMax Code sessions get messages through their listener".into(),
        )))
    }

    fn paste(&self) -> Paste {
        Paste::Numbered(
            " Images the user pasted in this session ([Image #N]): put the numbers in images to send them as attachments. N counts within the user's latest message with images.",
        )
    }

    fn pasted_image(&self, session_id: &str, n: u32) -> Result<PastedImage, String> {
        stored_image(session_id, n).ok_or_else(|| {
            format!("Couldn't find the pasted image [Image #{n}] in the session log; ask the user to paste it again, then send.")
        })
    }

    fn find(&self) -> Install {
        on_path("mcode")
    }

    fn changes(&self) -> String {
        format!(
            "MiniMax Code  {}  plugin imbo (tools + hooks); `imbo wait` allowed in {}",
            tilde(&plugins_dir().join("imbo")),
            tilde(&permission_file())
        )
    }

    /// MiniMax Code loads the plugin folder as soon as it appears; its
    /// hooks run from the next turn.
    fn connect(&self, home: &Home, _bin: &Path) -> Result<Connected, String> {
        add_plugin(home).map_err(|e| {
            format!(
                "Couldn't write {} ({e})",
                tilde(&plugins_dir().join("imbo"))
            )
        })?;
        let warning = match edit_permissions(&wait_rule(home), true) {
            Err(e) => Some(format!(
                "Couldn't edit {} ({e}); MiniMax Code will ask before each `imbo wait`",
                tilde(&permission_file())
            )),
            Ok(()) if old_storage() => Some(format!(
                "permission.storageWriteVersion is 1 in {}: MiniMax Code will save permissions in the old format, lose Imbo's rule and ask before each `imbo wait`",
                tilde(&data_dir().join("config.yaml"))
            )),
            Ok(()) => None,
        };
        Ok(Connected {
            detail: "plugin imbo: tools + hooks",
            warning,
        })
    }

    fn disconnect(&self, home: &Home, _bin: Option<&Path>) {
        let _ = std::fs::remove_dir_all(plugins_dir().join("imbo"));
        let _ = edit_permissions(&wait_rule(home), false);
    }
}

fn data_dir() -> PathBuf {
    user_home().join(".minimax")
}

fn plugins_dir() -> PathBuf {
    data_dir().join("plugins")
}

fn permission_file() -> PathBuf {
    data_dir().join("permission.json")
}

fn found(session_id: &str, online: bool) -> Found {
    let db = open_ro(&data_dir().join("v2/sqlite/runtime-state.sqlite"));
    Found {
        session_id: session_id.into(),
        title: db.and_then(|db| title(&db, session_id)).unwrap_or_default(),
        online,
    }
}

fn title(db: &rusqlite::Connection, session_id: &str) -> Option<String> {
    db.query_row(
        "SELECT title FROM local_runtime_sessions WHERE session_id = ?1",
        [session_id],
        |r| r.get::<_, Option<String>>(0),
    )
    .ok()
    .flatten()
}

/// Each running MiniMax Code process holds a lease file: its pid on the
/// first line, then `{"startToken":"darwin:<start>",…}`. Leases of killed
/// processes stay behind.
fn leases() -> Vec<(u32, Option<String>)> {
    let Ok(dir) = std::fs::read_dir(data_dir().join("v2/runtime-owner-leases")) else {
        return vec![];
    };
    dir.filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "owner"))
        .filter_map(|e| std::fs::read_to_string(e.path()).ok())
        .filter_map(|text| lease(&text))
        .collect()
}

fn lease(text: &str) -> Option<(u32, Option<String>)> {
    let mut lines = text.lines();
    let pid = lines.next()?.trim().parse().ok()?;
    let token = lines
        .next()
        .and_then(|l| serde_json::from_str::<Value>(l).ok())
        .and_then(|v| v["startToken"].as_str().map(str::to_string));
    Some((pid, token))
}

/// `pid` is the MiniMax Code process its lease names, not a later process
/// that got the same pid.
fn running(leases: &[(u32, Option<String>)], pid: u32) -> bool {
    crate::procs::alive(pid)
        && leases.iter().any(|(p, token)| {
            *p == pid
                && match (token, crate::procs::started(pid)) {
                    (Some(t), Some(s)) => t.rsplit(':').next() == Some(&s.to_string()),
                    _ => true,
                }
        })
}

/// The MiniMax Code process this hook or command runs under.
fn own_process() -> Option<u32> {
    let leases = leases();
    crate::procs::ancestors()
        .into_iter()
        .find(|p| running(&leases, *p))
}

/// The stop hook's request when no listener waits (API-LOCAL-011).
fn rearm() -> String {
    format!(
        "Imbo's message listener isn't running. Use the bash tool to run the command below in the background (run_in_background=true, timeout={WAIT_TIMEOUT_S}, description \"{}\"); don't wait for it to finish, and end this turn right after starting it without saying anything else: {}",
        super::WAKE,
        hook_command(&Home::resolve(), "wait")
    )
}

/// Write the plugin elsewhere and move it in whole: MiniMax Code loads a
/// plugin folder the moment it changes.
fn add_plugin(home: &Home) -> std::io::Result<()> {
    std::fs::create_dir_all(plugins_dir())?;
    let tmp = data_dir().join(format!(".imbo-plugin-{}", std::process::id()));
    let old = data_dir().join(format!(".imbo-plugin-old-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    write_plugin(home, &tmp)?;
    let target = plugins_dir().join("imbo");
    if target.exists() {
        std::fs::rename(&target, &old)?;
    }
    std::fs::rename(&tmp, &target)?;
    let _ = std::fs::remove_dir_all(&old);
    Ok(())
}

/// A plugin in Claude Code's format, the one whose pre-call hook may change
/// a call's input.
fn write_plugin(home: &Home, root: &Path) -> std::io::Result<()> {
    let write = |path: &str, v: Value| {
        let path = root.join(path);
        std::fs::create_dir_all(path.parent().expect("plugin files are in folders"))?;
        std::fs::write(path, serde_json::to_string_pretty(&v)? + "\n")
    };
    write(
        ".claude-plugin/plugin.json",
        json!({
            "name": "imbo",
            "version": crate::VERSION,
            "description": "Messages between agent sessions, across people and computers.",
        }),
    )?;
    write(
        ".mcp.json",
        json!({ "mcpServers": { "imbo": {
            "command": exe().to_string_lossy(),
            "args": ["mcp"],
            "env": server_env(home, imbo_proto::TOOL_MCODE),
            "timeout": CALL_TIMEOUT_MS,
        } } }),
    )?;
    let hook = |event: &str| {
        let command = hook_command(home, &format!("hook {} {event}", imbo_proto::TOOL_MCODE));
        json!([ { "type": "command", "command": command, "timeout": 10 } ])
    };
    let mut hooks = Map::new();
    for event in ["SessionStart", "SessionEnd", "UserPromptSubmit", "Stop"] {
        hooks.insert(event.into(), json!([ { "hooks": hook(event) } ]));
    }
    hooks.insert(
        "PreToolUse".into(),
        json!([ { "matcher": TOOL_MATCHER, "hooks": hook("PreToolUse") } ]),
    );
    write("hooks/hooks.json", json!({ "hooks": hooks }))
}

/// The permission rule that lets the agent run `imbo wait` as the stop hook
/// writes it.
fn wait_rule(home: &Home) -> Value {
    let pattern = format!("{}:*", hook_command(home, "wait"));
    json!({ "tool_name": "bash", "matcher": { "kind": "command", "pattern": pattern } })
}

/// Add `rule` to the permission file's allow list, or take it out, under
/// MiniMax Code's own lock: a `.lock` folder, stale after 10 seconds.
fn edit_permissions(rule: &Value, add: bool) -> Result<(), String> {
    let path = permission_file();
    if !add && !path.exists() {
        return Ok(());
    }
    std::fs::create_dir_all(data_dir()).map_err(|e| e.to_string())?;
    let lock = data_dir().join("permission.json.lock");
    let mut tries = 0;
    loop {
        match std::fs::create_dir(&lock) {
            Ok(()) => break,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && tries < 30 => {
                let stale = std::fs::metadata(&lock)
                    .and_then(|m| m.modified())
                    .ok()
                    .and_then(|t| t.elapsed().ok())
                    .is_some_and(|age| age > Duration::from_secs(10));
                if stale {
                    let _ = std::fs::remove_dir(&lock);
                } else {
                    std::thread::sleep(Duration::from_millis(100));
                }
                tries += 1;
            }
            Err(e) => return Err(format!("locked by MiniMax Code ({e})")),
        }
    }
    let result = (|| {
        let text = match std::fs::read_to_string(&path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e.to_string()),
        };
        match edit_rules(&text, rule, add)? {
            Some(v) => write_json(&path, &v),
            None => Ok(()),
        }
    })();
    let _ = std::fs::remove_dir(&lock);
    result
}

/// The permission file with `rule` added to or taken out of `allow`;
/// `None` when nothing changes. A file in the first format, without
/// `version`, is rewritten in the current one: MiniMax Code would read a
/// command rule written the old way as a path.
fn edit_rules(text: &str, rule: &Value, add: bool) -> Result<Option<Value>, String> {
    let mut v: Value = if text.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(text).map_err(|e| format!("not valid JSON: {e}"))?
    };
    let root = v.as_object_mut().ok_or("not a JSON object")?;
    match root.get("version") {
        Some(n) if n == 2 => {}
        None if add => upgrade(root)?,
        None => return Ok(None),
        Some(n) => return Err(format!("format version {n} isn't one Imbo knows")),
    }
    let allow = root
        .get_mut("allow")
        .and_then(Value::as_array_mut)
        .ok_or("`allow` isn't a list")?;
    match (add, allow.contains(rule)) {
        (true, false) => allow.push(rule.clone()),
        (false, true) => allow.retain(|r| r != rule),
        _ => return Ok(None),
    }
    Ok(Some(v))
}

/// Rewrite first-format rules, `tool(content)` text, the way MiniMax Code
/// reads them.
fn upgrade(root: &mut Map<String, Value>) -> Result<(), String> {
    let mut out = Map::new();
    out.insert("version".into(), json!(2));
    for key in ["allow", "deny", "ask"] {
        let rules = match root.remove(key) {
            None => vec![],
            Some(Value::Array(a)) => a,
            Some(_) => return Err(format!("`{key}` isn't a list")),
        };
        let rules = rules
            .into_iter()
            .map(|r| match r.as_str() {
                Some(text) => v2_rule(text),
                None => r,
            })
            .collect();
        out.insert(key.into(), Value::Array(rules));
    }
    out.append(root);
    *root = out;
    Ok(())
}

fn v2_rule(text: &str) -> Value {
    let (tool, content) = match text.split_once('(') {
        Some((tool, rest)) => (tool, rest.strip_suffix(')').unwrap_or(rest)),
        None => (text, ""),
    };
    let b = content.as_bytes();
    let path = ["/", "~/", "./", "../"]
        .iter()
        .any(|p| content.starts_with(p))
        || (b.len() > 2
            && b[0].is_ascii_alphabetic()
            && b[1] == b':'
            && matches!(b[2], b'/' | b'\\'));
    let matcher = if content.is_empty() {
        json!({ "kind": "tool" })
    } else if path {
        json!({ "kind": "path", "pattern": content, "actions": ["read", "write", "delete", "execute", "network"] })
    } else {
        json!({ "kind": "command", "pattern": content })
    };
    json!({ "tool_name": tool, "matcher": matcher })
}

/// `permission.storageWriteVersion: 1` in MiniMax Code's config.
fn old_storage() -> bool {
    std::fs::read_to_string(data_dir().join("config.yaml")).is_ok_and(|t| {
        t.lines().any(|l| {
            l.trim()
                .strip_prefix("storageWriteVersion:")
                .is_some_and(|v| v.trim() == "1")
        })
    })
}

/// Picture `n` of the session's latest prompt with pictures. MiniMax Code
/// logs every upload in `v2/assets/<year>/<month>/<day>/assets.jsonl`.
fn stored_image(session_id: &str, n: u32) -> Option<PastedImage> {
    let index = n.checked_sub(1)? as usize;
    for log in asset_logs() {
        let Ok(file) = std::fs::File::open(&log) else {
            continue;
        };
        let pictures: Vec<Value> = std::io::BufReader::new(file)
            .lines()
            .map_while(Result::ok)
            .filter_map(|l| serde_json::from_str::<Value>(&l).ok())
            .filter(|v| v["sessionId"] == session_id && v["kind"] == "image")
            .collect();
        let Some(latest) = pictures
            .iter()
            .max_by_key(|v| v["createdAtMs"].as_i64())
            .map(|v| v["turnId"].clone())
        else {
            continue;
        };
        let pic = pictures
            .iter()
            .filter(|v| v["turnId"] == latest)
            .nth(index)?;
        return Some(PastedImage {
            media_type: pic["mimeType"].as_str().unwrap_or("image/png").into(),
            bytes: std::fs::read(pic["absolutePath"].as_str()?).ok()?,
        });
    }
    None
}

/// Upload logs, newest day first.
fn asset_logs() -> Vec<PathBuf> {
    let subdirs = |p: &Path| -> Vec<PathBuf> {
        let mut v: Vec<PathBuf> = std::fs::read_dir(p)
            .map(|d| {
                d.filter_map(|e| Some(e.ok()?.path()))
                    .filter(|p| p.is_dir())
                    .collect()
            })
            .unwrap_or_default();
        v.sort();
        v.reverse();
        v
    };
    let mut logs = vec![];
    for year in subdirs(&data_dir().join("v2/assets")) {
        for month in subdirs(&year) {
            for day in subdirs(&month) {
                let log = day.join("assets.jsonl");
                if log.is_file() {
                    logs.push(log);
                }
            }
        }
    }
    logs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule() -> Value {
        json!({ "tool_name": "bash", "matcher": { "kind": "command", "pattern": "/x/imbo wait:*" } })
    }

    #[test]
    fn wait_rule_goes_into_new_and_current_permission_files() {
        let fresh = edit_rules("", &rule(), true).unwrap().unwrap();
        assert_eq!(
            fresh,
            json!({ "version": 2, "allow": [rule()], "deny": [], "ask": [] })
        );
        assert_eq!(edit_rules(&fresh.to_string(), &rule(), true).unwrap(), None);
        let removed = edit_rules(&fresh.to_string(), &rule(), false)
            .unwrap()
            .unwrap();
        assert_eq!(removed["allow"], json!([]));
        assert_eq!(edit_rules("", &rule(), false).unwrap(), None);
        assert!(edit_rules(r#"{"version":3,"allow":[]}"#, &rule(), true).is_err());
        assert!(edit_rules("{ nope", &rule(), true).is_err());
    }

    #[test]
    fn first_format_rules_are_rewritten_as_minimax_code_reads_them() {
        let old = r#"{"allow":["bash(git status:*)","read(/etc/hosts)","webfetch"],"deny":["bash(rm:*)"],"theme":"dark"}"#;
        let v = edit_rules(old, &rule(), true).unwrap().unwrap();
        assert_eq!(
            v,
            json!({
                "version": 2,
                "allow": [
                    { "tool_name": "bash", "matcher": { "kind": "command", "pattern": "git status:*" } },
                    { "tool_name": "read", "matcher": { "kind": "path", "pattern": "/etc/hosts",
                        "actions": ["read", "write", "delete", "execute", "network"] } },
                    { "tool_name": "webfetch", "matcher": { "kind": "tool" } },
                    rule()
                ],
                "deny": [ { "tool_name": "bash", "matcher": { "kind": "command", "pattern": "rm:*" } } ],
                "ask": [],
                "theme": "dark"
            })
        );
        let keys: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys[0], "version");
    }

    #[test]
    fn reads_lease_files() {
        let text =
            "77253\n{\"schemaVersion\":1,\"pid\":77253,\"startToken\":\"darwin:1792956572\"}\n";
        assert_eq!(lease(text), Some((77253, Some("darwin:1792956572".into()))));
        assert_eq!(lease("x\n"), None);
        let me = std::process::id();
        let start = crate::procs::started(me).map(|s| format!("darwin:{s}"));
        assert!(running(&[(me, start)], me));
        assert!(
            !running(&[(me, Some("darwin:1".into()))], me) || crate::procs::started(me).is_none()
        );
        assert!(!running(&[], me));
    }
}
