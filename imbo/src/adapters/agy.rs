//! Antigravity CLI (`agy`): a plugin Imbo writes gives every session Imbo's
//! MCP server and hooks. Messages go in through the agent API of the `agy`
//! process, which starts a turn in an idle session and joins the one that
//! is running. Only commands the agent runs are told that API's address and
//! token, so the stop hook asks the agent, up to three times per `agy`
//! process, to run `imbo hook antigravity attach`, which permission rules
//! let run inside the terminal sandbox; until then messages come with the
//! next model call (ADR-068).

use std::path::{Path, PathBuf};
use std::time::Duration;

use futures::future::BoxFuture;
use imbo_proto::api::MessageView;
use imbo_proto::local::LocalFile;
use serde_json::{Map, Value, json};
use tracing::{info, warn};

use super::{
    Adapter, Connected, Found, Install, Paste, PastedImage, Registration, drop_empty, edit_json,
    env, hook_command, object, on_path, remembered_all, server_env, write_json,
};
use crate::paths::{Home, exe, tilde, user_home};
use crate::render;

const TOOL: &str = imbo_proto::TOOL_AGY;
/// Hook context longer than this is cut.
const CONTEXT_LIMIT: usize = 9000;
/// The plugin's folder, and its MCP server's key.
const PLUGIN: &str = "imbo";
/// Antigravity CLI names a plugin's MCP server `<plugin>_<server>`; this
/// rule lets its tools run without asking.
const TOOLS_RULE: &str = "mcp(imbo_imbo/*)";
/// Tries per insert; after that the process's sessions get their messages
/// with the next model call.
const MAX_INJECT_FAILURES: u32 = 3;
/// Turns whose end may ask the agent to run `attach`, per `agy` process.
const MAX_ASKS: u64 = 3;
const INJECT_TIMEOUT: Duration = Duration::from_secs(10);

pub static AGY: Agy = Agy;

pub struct Agy;

impl Adapter for Agy {
    fn id(&self) -> &'static str {
        TOOL
    }

    fn name(&self) -> &'static str {
        "Antigravity CLI"
    }

    /// Calls of Imbo's tools name the conversation and the one the user
    /// opened; a subagent's calls count as the latter's.
    fn session(&self, meta: &[&Map<String, Value>]) -> Option<String> {
        [
            "antigravity.google/root_conversation_id",
            "antigravity.google/conversation_id",
        ]
        .iter()
        .find_map(|key| {
            meta.iter().find_map(|m| {
                let id = m.get(*key)?.as_str()?;
                (!id.is_empty()).then(|| id.to_string())
            })
        })
        .or_else(command_session)
    }

    fn mcp_client(&self) -> Option<&'static str> {
        Some("antigravity-client")
    }

    fn marked(&self) -> bool {
        env("ANTIGRAVITY_AGENT").as_deref() == Some("1")
    }

    fn env_pass(&self) -> &'static [&'static str] {
        &[]
    }

    fn env_session(&self) -> &'static [&'static str] {
        &[
            "ANTIGRAVITY_AGENT",
            "ANTIGRAVITY_CONVERSATION_ID",
            "ANTIGRAVITY_TRAJECTORY_ID",
            "ANTIGRAVITY_SOURCE_METADATA",
            "ANTIGRAVITY_LS_ADDRESS",
            "ANTIGRAVITY_CSRF_TOKEN",
        ]
    }

    /// The MCP server runs under the `agy` process. Runs with `-p` don't
    /// register through the hooks, so a call of theirs does it here.
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
        super::remember(TOOL, r.pid, &r.session_id);
        Some(found(&r.session_id, crate::procs::alive(r.pid)))
    }

    fn adopt(&self, session_id: &str) -> Option<Found> {
        let pid = process_of(session_id)?;
        Some(found(session_id, crate::procs::alive(pid)))
    }

    fn poll_every(&self) -> Duration {
        Duration::from_secs(5)
    }

    /// The session each running `agy` process has open. The hooks report
    /// it, but none runs before the user's first prompt: until then the
    /// conversation is the one whose lock file the process holds.
    fn discover(&self) -> BoxFuture<'_, Result<Vec<Found>, String>> {
        Box::pin(async {
            let open = tokio::task::spawn_blocking(|| {
                let mut open = remembered_all(TOOL);
                let presence = presence_dir();
                for pid in crate::procs::pids() {
                    if open.iter().any(|(p, _)| *p == pid)
                        || crate::procs::name(pid).as_deref() != Some("agy")
                        || print_mode(&crate::procs::args(pid))
                    {
                        continue;
                    }
                    if let Some(id) = locked_session(&crate::procs::open_files(pid), &presence) {
                        super::remember(TOOL, pid, &id);
                        open.push((pid, id));
                    }
                }
                open
            })
            .await
            .map_err(|e| e.to_string())?;
            forget_links(&open);
            Ok(open.into_iter().map(|(_, id)| found(&id, true)).collect())
        })
    }

    fn channel(&self, session_id: &str) -> &'static str {
        match process_of(session_id).and_then(link) {
            Some(l) if l.usable() => "inject",
            _ => "prompt_hook",
        }
    }

    /// Until `attach` has run in the session's `agy` process, its messages
    /// come with the next model call (API-LOCAL-003).
    fn ready(&self, session_id: &str) -> Result<(), &'static str> {
        match self.channel(session_id) {
            "inject" => Ok(()),
            _ => Err("prompt-hook-fallback"),
        }
    }

    fn hook<'a>(&'a self, event: &'a str, input: &'a Value) -> BoxFuture<'a, Option<String>> {
        Box::pin(async move {
            if event == "attach" {
                return Some(attach());
            }
            let session_id = input["conversationId"].as_str().filter(|s| !s.is_empty())?;
            // Subagents run the hooks too, each under a conversation of
            // its own that the user never sees.
            let data = input["artifactDirectoryPath"]
                .as_str()
                .and_then(|p| Path::new(p).parent()?.parent().map(Path::to_path_buf))
                .unwrap_or_else(data_dir);
            if !opened_by_user(&data, session_id) {
                return None;
            }
            let pid = super::tool_process().unwrap_or_else(|| unsafe { libc::getppid() } as u32);
            // A run with `-p` ends with its answer: no messages for it.
            if print_mode(&crate::procs::args(pid)) {
                return None;
            }
            super::hook_register(TOOL, session_id, pid).await;
            match event {
                "SessionStart" => Some(inject(&note())),
                "PreInvocation" => super::hook_context(TOOL, session_id, CONTEXT_LIMIT)
                    .await
                    .map(|c| inject(&c)),
                // Asked once per turn, and only when the turn ended on its
                // own with nothing left running: an agent waiting for a
                // background command would take the request for noise.
                "Stop" => {
                    let asked = link(pid).map_or(Some(0), |l| l.asked())?;
                    if asked >= MAX_ASKS
                        || input["executionNum"].as_u64().unwrap_or(0) != 0
                        || input["continuationActive"].as_bool() == Some(true)
                        || input["fullyIdle"].as_bool() == Some(false)
                        || input["terminationReason"].as_str() != Some("NO_TOOL_CALL")
                    {
                        return None;
                    }
                    save_link(pid, &json!({ "asked": asked + 1 }));
                    Some(json!({ "decision": "continue", "reason": attach_request() }).to_string())
                }
                _ => None,
            }
        })
    }

    fn deliver<'a>(
        &'a self,
        session_id: &'a str,
        m: &'a MessageView,
        files: &'a [LocalFile],
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            if super::fault(TOOL) {
                return Err("fault injected".into());
            }
            send(session_id, &render::for_codex(m, files)).await?;
            info!(msg = m.id, session = %session_id, "delivered into Antigravity CLI session");
            Ok(())
        })
    }

    fn note<'a>(&'a self, session_id: &'a str, text: &'a str) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(send(session_id, text))
    }

    fn paste(&self) -> Paste {
        Paste::Path(" To send an image the user pasted, put its file path in files.")
    }

    fn pasted_image(&self, _session_id: &str, _n: u32) -> Result<PastedImage, String> {
        Err("Antigravity CLI sessions send pasted images by path, in files.".into())
    }

    fn find(&self) -> Install {
        on_path("agy")
    }

    fn changes(&self) -> String {
        format!(
            "Antigravity CLI  {}  plugin imbo; {}  permissions.allow for Imbo's tools and `imbo hook antigravity attach`",
            tilde(&plugin_dir()),
            tilde(&settings_file())
        )
    }

    /// Antigravity CLI reads its plugins when a session starts.
    fn connect(&self, home: &Home, _bin: &Path) -> Result<Connected, String> {
        let dir = plugin_dir();
        for (name, v) in plugin_files(home) {
            let path = dir.join(name);
            write_json(&path, &v).map_err(|e| format!("Couldn't write {} ({e})", tilde(&path)))?;
        }
        // The folders the rules name: the sandbox knows them by their real
        // paths.
        for dir in [links_dir(home), pids_dir(home)] {
            std::fs::create_dir_all(&dir)
                .map_err(|e| format!("Couldn't create {} ({e})", tilde(&dir)))?;
        }
        let rules = allow_rules(home);
        let by_hand = format!("\"permissions\": {{\"allow\": {}}}", json!(rules));
        let warning = edit_json(&settings_file(), &by_hand, |root| {
            let allow = object(root, "permissions")?
                .entry("allow")
                .or_insert_with(|| json!([]))
                .as_array_mut()
                .ok_or("`permissions.allow` isn't a list")?;
            allow.retain(|r| !our_rule(r, home, &rules));
            allow.extend(rules.iter().map(|r| json!(r)));
            Ok(())
        })
        .err();
        Ok(Connected {
            detail: "plugin imbo: MCP server + hooks",
            warning,
        })
    }

    fn disconnect(&self, home: &Home, _bin: Option<&Path>) {
        let _ = std::fs::remove_dir_all(plugin_dir());
        let _ = edit_json(&settings_file(), "", |root| {
            if let Some(Value::Object(permissions)) = root.get_mut("permissions") {
                if let Some(Value::Array(allow)) = permissions.get_mut("allow") {
                    let rules = allow_rules(home);
                    allow.retain(|r| !our_rule(r, home, &rules));
                }
                drop_empty(permissions, "allow");
            }
            drop_empty(root, "permissions");
            Ok(())
        });
    }
}

/// Hand `text` to the agent API of the session's `agy` process. Tried 3
/// times; after that the process's sessions get their messages with the
/// next model call.
async fn send(session_id: &str, text: &str) -> Result<(), String> {
    let Some((pid, l)) =
        process_of(session_id).and_then(|pid| Some((pid, link(pid).filter(Link::usable)?)))
    else {
        return Err("the session's agy process hasn't run attach".into());
    };
    // The process's own program, not one the sandbox could name.
    let Some(agy) = crate::procs::exe(pid) else {
        return Err("the session's agy process has exited".into());
    };
    let mut last = String::new();
    for attempt in 0..MAX_INJECT_FAILURES {
        match send_message(&agy, &l, session_id, text).await {
            Ok(()) => return Ok(()),
            Err(e) => {
                warn!(session = %session_id, attempt = attempt + 1, error = %e, "Antigravity CLI insert failed");
                last = e;
                tokio::time::sleep(Duration::from_millis(300)).await;
            }
        }
    }
    let mut v = l.0.clone();
    v["failed"] = json!(true);
    save_link(pid, &v);
    warn!(session = %session_id, "insert failed 3 times; this agy process's sessions now get messages with the next model call");
    Err(last)
}

/// `agy agentapi send-message` puts a message into a conversation; the
/// address and token reach it through its environment.
async fn send_message(agy: &Path, l: &Link, session_id: &str, text: &str) -> Result<(), String> {
    let mut cmd = tokio::process::Command::new(agy);
    cmd.args(["agentapi", "send-message", session_id, text])
        .env("ANTIGRAVITY_LS_ADDRESS", l.text("address"))
        .env("ANTIGRAVITY_CSRF_TOKEN", l.text("token"))
        // The API listens on this computer.
        .env("NO_PROXY", "localhost,127.0.0.1,::1")
        .env("no_proxy", "localhost,127.0.0.1,::1")
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);
    let out = tokio::time::timeout(INJECT_TIMEOUT, cmd.output())
        .await
        .map_err(|_| "timed out".to_string())?
        .map_err(|e| e.to_string())?;
    let answer: Value = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
    if let Some(e) = answer["error"].as_str() {
        return Err(e.to_string());
    }
    if !out.status.success() || answer["response"].is_null() {
        let said = String::from_utf8_lossy(&out.stderr);
        return Err(said.lines().last().unwrap_or("no answer").to_string());
    }
    Ok(())
}

/// Where Antigravity CLI reads its customizations.
fn config_dir() -> PathBuf {
    user_home().join(".gemini/config")
}

/// Where Antigravity CLI keeps its conversations and settings; it tells
/// the commands it runs.
fn data_dir() -> PathBuf {
    env("ANTIGRAVITY_APP_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| user_home().join(".gemini/antigravity-cli"))
}

fn plugin_dir() -> PathBuf {
    config_dir().join("plugins").join(PLUGIN)
}

fn settings_file() -> PathBuf {
    data_dir().join("settings.json")
}

/// The plugin's files, by name.
fn plugin_files(home: &Home) -> [(&'static str, Value); 3] {
    let handler = |event: &str| {
        json!({
            "type": "command",
            "command": hook_command(home, &format!("hook {TOOL} {event}")),
            "timeout": 10,
        })
    };
    [
        ("plugin.json", json!({ "name": PLUGIN })),
        (
            "mcp_config.json",
            json!({ "mcpServers": { PLUGIN: {
                "command": exe().to_string_lossy(),
                "args": ["mcp"],
                "env": server_env(home, TOOL),
            } } }),
        ),
        (
            "hooks.json",
            json!({ PLUGIN: {
                "SessionStart": [handler("SessionStart")],
                "PreInvocation": [handler("PreInvocation")],
                "Stop": [handler("Stop")],
            } }),
        ),
    ]
}

/// Antigravity CLI keeps a lock file for each conversation a user or a
/// `-p` run opened, and none for a subagent's.
fn opened_by_user(data: &Path, session_id: &str) -> bool {
    data.join("presence")
        .join(format!("{session_id}.lock"))
        .exists()
}

/// The lock files' folder as the system names it in a process's open files,
/// links in the path followed.
fn presence_dir() -> PathBuf {
    let dir = data_dir().join("presence");
    std::fs::canonicalize(&dir).unwrap_or(dir)
}

/// The conversation whose lock file is the only one in `presence` among
/// `files`, a process's open files. With several open, which one the process
/// shows isn't known: its hooks say.
fn locked_session(files: &[PathBuf], presence: &Path) -> Option<String> {
    let mut ids = files
        .iter()
        .filter(|f| f.parent() == Some(presence) && f.extension().is_some_and(|e| e == "lock"))
        .filter_map(|f| f.file_stem()?.to_str());
    let id = ids.next()?;
    ids.all(|other| other == id).then(|| id.to_string())
}

/// The session behind a command the agent runs. A subagent's commands name
/// its own conversation: they count as the session the `agy` process above
/// has open.
fn command_session() -> Option<String> {
    let id = env("ANTIGRAVITY_CONVERSATION_ID")?;
    if opened_by_user(&data_dir(), &id) {
        return Some(id);
    }
    Some(super::ancestor_session(TOOL).map_or(id, |(_, s)| s))
}

/// `agy` was started to answer one prompt and exit.
fn print_mode(args: &[String]) -> bool {
    args.iter()
        .skip(1)
        .take_while(|a| a.as_str() != "--")
        .filter_map(|a| a.strip_prefix('-'))
        .map(|a| a.strip_prefix('-').unwrap_or(a))
        .map(|a| a.split_once('=').map_or(a, |(name, _)| name))
        .any(|name| matches!(name, "p" | "print" | "prompt"))
}

/// The `agy` process that has `session_id` open.
fn process_of(session_id: &str) -> Option<u32> {
    remembered_all(TOOL)
        .into_iter()
        .find(|(_, s)| s == session_id)
        .map(|(pid, _)| pid)
}

fn found(session_id: &str, online: bool) -> Found {
    Found {
        session_id: session_id.into(),
        title: title(&data_dir(), session_id).unwrap_or_default(),
        online,
    }
}

/// The title the user gave the conversation, else the one Antigravity CLI
/// made up for it.
fn title(data: &Path, session_id: &str) -> Option<String> {
    let named = std::fs::read_to_string(data.join(format!("annotations/{session_id}.pbtxt")))
        .ok()
        .and_then(|t| annotated_title(&t));
    named.or_else(|| {
        let cache = std::fs::read_to_string(data.join("cache/conversation_metadata.json")).ok()?;
        let v: Value = serde_json::from_str(&cache).ok()?;
        v["conversations"][session_id]["summary"]["Title"]
            .as_str()
            .map(|t| t.trim().to_string())
            .filter(|t| !t.is_empty())
    })
}

/// `title` in a conversation's annotations, a protobuf text file: other
/// than ASCII is written as octal bytes.
fn annotated_title(text: &str) -> Option<String> {
    let quoted = text
        .lines()
        .find_map(|l| l.trim().strip_prefix("title:"))?
        .trim()
        .strip_prefix('"')?
        .strip_suffix('"')?;
    let mut bytes = vec![];
    let mut rest = quoted.as_bytes();
    while let Some((&b, tail)) = rest.split_first() {
        rest = tail;
        if b != b'\\' {
            bytes.push(b);
            continue;
        }
        let Some((&e, tail)) = rest.split_first() else {
            break;
        };
        rest = tail;
        match e {
            b'n' => bytes.push(b'\n'),
            b't' => bytes.push(b'\t'),
            b'r' => bytes.push(b'\r'),
            b'0'..=b'7' => {
                let mut n = u32::from(e - b'0');
                for _ in 0..2 {
                    match rest.split_first() {
                        Some((&d @ b'0'..=b'7', tail)) => {
                            n = n * 8 + u32::from(d - b'0');
                            rest = tail;
                        }
                        _ => break,
                    }
                }
                bytes.push(n as u8);
            }
            other => bytes.push(other),
        }
    }
    let title = String::from_utf8_lossy(&bytes).trim().to_string();
    (!title.is_empty()).then_some(title)
}

/// What Imbo keeps for one `agy` process: the times its stop hook asked
/// for `attach`, then the agent API `attach` found (address and token),
/// and whether inserts through it kept failing. The agent can write this
/// file from inside the terminal sandbox, so nothing in it is run.
struct Link(Value);

impl Link {
    fn text(&self, key: &str) -> &str {
        self.0[key].as_str().unwrap_or_default()
    }

    /// Attached to an API on this computer, and not given up on.
    fn usable(&self) -> bool {
        local(self.text("address")) && self.0["failed"].as_bool() != Some(true)
    }

    /// How often the stop hook asked; `None` once `attach` has run.
    fn asked(&self) -> Option<u64> {
        self.text("address")
            .is_empty()
            .then(|| self.0["asked"].as_u64().unwrap_or(0))
    }
}

/// `host:port` on this computer, the only place an `agy` process listens.
fn local(address: &str) -> bool {
    address.rsplit_once(':').is_some_and(|(host, port)| {
        matches!(host, "localhost" | "127.0.0.1" | "[::1]")
            && !port.is_empty()
            && port.parse::<u16>().is_ok()
    })
}

fn links_dir(home: &Home) -> PathBuf {
    home.state().join(TOOL)
}

/// `super::pids_dir` of `home`.
fn pids_dir(home: &Home) -> PathBuf {
    home.state().join("pids").join(TOOL)
}

fn link(pid: u32) -> Option<Link> {
    let path = links_dir(&Home::resolve()).join(format!("{pid}.json"));
    serde_json::from_str(&std::fs::read_to_string(path).ok()?)
        .ok()
        .map(Link)
}

/// Kept private: the token lets its holder put messages into the session.
fn save_link(pid: u32, v: &Value) -> bool {
    let dir = links_dir(&Home::resolve());
    let _ = std::fs::create_dir_all(&dir);
    crate::paths::write_private(&dir.join(format!("{pid}.json")), v.to_string().as_bytes()).is_ok()
}

/// Drop what is kept for `agy` processes that have exited.
fn forget_links(open: &[(u32, String)]) {
    let Ok(dir) = std::fs::read_dir(links_dir(&Home::resolve())) else {
        return;
    };
    for e in dir.filter_map(|e| e.ok()) {
        let pid = e
            .path()
            .file_stem()
            .and_then(|n| n.to_str()?.parse::<u32>().ok());
        let gone = match pid {
            Some(pid) => !open.iter().any(|(p, _)| *p == pid) && !crate::procs::alive(pid),
            None => false,
        };
        if gone {
            let _ = std::fs::remove_file(e.path());
        }
    }
}

/// `imbo hook antigravity attach`, run by the agent: its environment names
/// the agent API of the `agy` process above. Nothing here needs the
/// daemon, which the terminal sandbox hides. Returns what the agent reads.
fn attach() -> String {
    let (Some(address), Some(token)) =
        (env("ANTIGRAVITY_LS_ADDRESS"), env("ANTIGRAVITY_CSRF_TOKEN"))
    else {
        return "Imbo: this command only works when an Antigravity CLI agent runs it.\n".into();
    };
    let Some(pid) = super::ancestor_session(TOOL)
        .map(|(pid, _)| pid)
        .or_else(super::tool_process)
    else {
        return "Imbo: couldn't find the Antigravity CLI process.\n".into();
    };
    let saved = save_link(pid, &json!({ "address": address, "token": token }));
    if saved {
        "Imbo: connected. Messages now reach this session as they arrive.\n".into()
    } else {
        "Imbo: the terminal sandbox kept this command from Imbo's files. Run it once more outside the sandbox.\n".into()
    }
}

/// The command the stop hook asks for, the same every time so one
/// permission rule covers it. `imbo hook` reads its input to the end.
fn attach_command(home: &Home) -> String {
    format!(
        "{} </dev/null",
        hook_command(home, &format!("hook {TOOL} attach"))
    )
}

/// What `imbo setup` adds to `permissions.allow`: Imbo's tools and
/// `attach_command` run without asking, and the terminal sandbox, which
/// hides all but the workspace, shows `attach` the three paths it needs:
/// `imbo` itself, the sessions the hooks reported and the folder it writes.
fn allow_rules(home: &Home) -> Vec<String> {
    let real = |p: PathBuf| std::fs::canonicalize(&p).unwrap_or(p);
    let mut rules = vec![TOOLS_RULE.to_string(), attach_rule(home)];
    let imbo = exe();
    for path in [real(imbo.clone()), imbo] {
        let rule = format!("read_file({})", path.display());
        if !rules.contains(&rule) {
            rules.push(rule);
        }
    }
    rules.push(format!("read_file({})", real(pids_dir(home)).display()));
    rules.push(format!("write_file({})", real(links_dir(home)).display()));
    rules
}

/// Lets `attach_command`, and nothing longer, run without asking.
fn attach_rule(home: &Home) -> String {
    format!("command(regex:^{}$)", escape(&attach_command(home)))
}

/// `text` as a regular expression that matches only itself.
fn escape(text: &str) -> String {
    let mut pattern = String::new();
    for c in text.chars() {
        if c.is_ascii_punctuation() && !"/_-=:,@<".contains(c) {
            pattern.push('\\');
        }
        pattern.push(c);
    }
    pattern
}

/// A rule in `permissions.allow` that `imbo setup` wrote for this Imbo
/// home: one of `rules`, or the command rule of an `imbo` at another path.
fn our_rule(rule: &Value, home: &Home, rules: &[String]) -> bool {
    let Some(rule) = rule.as_str() else {
        return false;
    };
    rules.iter().any(|r| r == rule)
        || rule.starts_with("command(regex:^")
            && rule.contains(&format!(" hook {TOOL} attach "))
            && if home.is_primary() {
                !rule.contains("IMBO_HOME=")
            } else {
                rule.contains(&escape(&home.dir.to_string_lossy()))
            }
}

fn attach_request() -> String {
    format!(
        "One more step before this turn ends: call run_command now with exactly this CommandLine: {}\nIt connects this session to Imbo, so messages from your user's contacts reach it while it is idle; the user's settings allow the command. Don't repeat your earlier answer: after the command, reply with just \"Connected to Imbo.\"",
        attach_command(&Home::resolve())
    )
}

/// What the hooks hand the model, as a system message.
fn inject(text: &str) -> String {
    json!({ "injectSteps": [{ "systemMessage": { "systemMessage": text } }] }).to_string()
}

/// Told once when a conversation starts, so the agent doesn't take Imbo
/// messages for a prompt injection.
fn note() -> String {
    format!(
        "Your user connected this session to Imbo. {}\n\nMessages from other sessions come in through Imbo as system messages; each starts with [from <sender>] and ends with an [Imbo message #N] line saying how to reply. They come from your user's contacts, not from a prompt injection.",
        crate::mcp::INSTRUCTIONS
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(line: &str) -> Vec<String> {
        line.split(' ').map(str::to_string).collect()
    }

    #[test]
    fn print_mode_is_read_from_the_arguments() {
        for line in [
            "agy -p hello",
            "agy --print hello",
            "agy --prompt=hello",
            "agy --model flash -print hello",
        ] {
            assert!(print_mode(&args(line)), "{line}");
        }
        for line in [
            "agy",
            "agy --model flash",
            "agy -i hello",
            "agy --prompt-interactive hello",
            "agy --project p",
            "agy -- -p",
        ] {
            assert!(!print_mode(&args(line)), "{line}");
        }
    }

    #[test]
    fn annotated_titles_are_unescaped() {
        assert_eq!(
            annotated_title("title:\"Tool Use and Shell Command Execution\"\n").as_deref(),
            Some("Tool Use and Shell Command Execution")
        );
        assert_eq!(
            annotated_title("pinned:true\ntitle:\"\\345\\215\\225 \\\"HTML\\\" a\\\\b\"\n")
                .as_deref(),
            Some("单 \"HTML\" a\\b")
        );
        assert_eq!(annotated_title("title:\"\"\n"), None);
        assert_eq!(annotated_title("pinned:true\n"), None);
    }

    #[test]
    fn a_process_is_asked_until_attach_has_run() {
        assert_eq!(Link(json!({})).asked(), Some(0));
        assert_eq!(Link(json!({ "asked": 2 })).asked(), Some(2));
        assert!(!Link(json!({ "asked": 2 })).usable());
        let attached = json!({ "address": "localhost:1", "token": "t" });
        assert_eq!(Link(attached.clone()).asked(), None);
        assert!(Link(attached.clone()).usable());
        let mut failed = attached;
        failed["failed"] = json!(true);
        assert!(!Link(failed.clone()).usable());
        assert_eq!(Link(failed).asked(), None);
        for address in ["127.0.0.1:64454", "[::1]:8080"] {
            assert!(Link(json!({ "address": address })).usable(), "{address}");
        }
        for address in [
            "example.com:443",
            "localhost",
            "localhost:",
            "localhost:x",
            "10.0.0.2:80",
        ] {
            assert!(!Link(json!({ "address": address })).usable(), "{address}");
        }
    }

    #[test]
    fn the_plugin_carries_the_server_and_the_hooks() {
        let home = Home::resolve();
        let files: std::collections::BTreeMap<_, _> = plugin_files(&home).into_iter().collect();
        assert_eq!(files["plugin.json"]["name"], PLUGIN);
        let server = &files["mcp_config.json"]["mcpServers"][PLUGIN];
        assert_eq!(server["args"], json!(["mcp"]));
        assert_eq!(server["env"]["IMBO_HOST"], TOOL);
        let hooks = &files["hooks.json"][PLUGIN];
        for event in ["SessionStart", "PreInvocation", "Stop"] {
            let command = hooks[event][0]["command"].as_str().unwrap();
            assert!(
                command.ends_with(&format!("hook {TOOL} {event}")),
                "{command}"
            );
        }
        assert_eq!(TOOLS_RULE, format!("mcp({PLUGIN}_{PLUGIN}/*)"));
    }

    #[test]
    fn the_attach_rule_matches_the_command_and_nothing_longer() {
        let home = Home::resolve();
        let rule = attach_rule(&home);
        let command = attach_command(&home);
        assert!(command.ends_with(&format!(" hook {TOOL} attach </dev/null")));
        let pattern = rule
            .strip_prefix("command(regex:^")
            .and_then(|r| r.strip_suffix("$)"))
            .unwrap();
        // Unescaped, the pattern is the command itself.
        let mut plain = String::new();
        let mut chars = pattern.chars();
        while let Some(c) = chars.next() {
            plain.push(if c == '\\' { chars.next().unwrap() } else { c });
        }
        assert_eq!(plain, command);
        assert!(!pattern.replace("\\.", "").contains('.'));
        let rules = allow_rules(&home);
        assert_eq!(&rules[..2], [TOOLS_RULE.to_string(), rule]);
        assert!(rules[2..].iter().all(|r| r.contains("_file(/")));
        assert!(rules.last().unwrap().starts_with("write_file("));
        for r in &rules {
            assert!(our_rule(&json!(r), &home, &rules), "{r}");
        }
        // The command rule `imbo setup` wrote when `imbo` was elsewhere.
        let moved = rules[1].replace("/imbo", "/old/imbo");
        assert!(our_rule(&json!(moved), &home, &rules));
        for theirs in ["command(git push)", "mcp(other/*)", "read_file(/var/log)"] {
            assert!(!our_rule(&json!(theirs), &home, &rules), "{theirs}");
        }
        // A test home, whose path the rule writes escaped.
        let other = Home {
            dir: PathBuf::from("/tmp/imbo-t/.bob"),
        };
        let moved = attach_rule(&other).replace("/imbo ", "/old/imbo ");
        assert!(our_rule(&json!(moved), &other, &allow_rules(&other)));
        assert!(!our_rule(&json!(moved), &home, &rules));
    }

    #[test]
    fn a_process_holding_one_lock_has_that_conversation_open() {
        let presence = Path::new("/data/presence");
        let files = |names: &[&str]| names.iter().map(PathBuf::from).collect::<Vec<_>>();
        let one = files(&[
            "/data/conversations/aaa.db-shm",
            "/data/presence",
            "/data/presence/aaa.lock",
            "/data/presence/aaa.lock",
            "/elsewhere/presence/bbb.lock",
            "/data/presence/notes.txt",
        ]);
        assert_eq!(locked_session(&one, presence).as_deref(), Some("aaa"));
        let two = files(&["/data/presence/aaa.lock", "/data/presence/bbb.lock"]);
        assert_eq!(locked_session(&two, presence), None);
        assert_eq!(
            locked_session(&files(&["/data/log/cli.log"]), presence),
            None
        );
    }

    #[test]
    fn a_subagents_conversation_has_no_lock() {
        let dir = std::env::temp_dir().join(format!("imbo-agy-test-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("presence")).unwrap();
        std::fs::write(dir.join("presence/main.lock"), "").unwrap();
        assert!(opened_by_user(&dir, "main"));
        assert!(!opened_by_user(&dir, "sub"));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
