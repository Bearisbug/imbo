//! One adapter per tool (ADR-016). Everything Imbo does differently for a
//! tool lives in its adapter; the rest of the client finds adapters through
//! `all()` and never names a tool.

pub mod claude;
pub mod codex;
pub mod cursor;
pub mod hermes;
pub mod kimi;
pub mod mcode;
pub mod opencode;
pub mod pi;
pub mod zcode;

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use futures::future::BoxFuture;
use imbo_proto::api::MessageView;
use imbo_proto::local::{ListenerStatus, LocalFile, LocalRequest};
use serde_json::{Map, Value, json};

use crate::local::Local;
use crate::paths::Home;

/// Every tool Imbo works with. Without `IMBO_HOST` a session is recognised
/// by the first adapter that claims it, so the order matters when two tools
/// leave the same marks: ZCode sets Claude Code's session variable for its
/// hooks, and tools started from a Claude Code or Codex shell inherit theirs
/// (ADR-018).
static ALL: [&dyn Adapter; 9] = [
    &zcode::ZCODE,
    &cursor::CURSOR,
    &mcode::MCODE,
    &kimi::KIMI,
    &pi::PI,
    &hermes::HERMES,
    &opencode::OPENCODE,
    &claude::CLAUDE,
    &codex::CODEX,
];

pub fn all() -> &'static [&'static dyn Adapter] {
    &ALL
}

/// The adapter for a tool ID, `None` for tools Imbo doesn't know.
pub fn get(id: &str) -> Option<&'static dyn Adapter> {
    ALL.iter().copied().find(|a| a.id() == id)
}

/// Who runs an `imbo mcp` process (ADR-018).
pub enum Host {
    Tool(&'static dyn Adapter),
    /// Another tool imported a config Imbo wrote for a different one, such
    /// as Cursor loading the Claude Code plugin: it gets no tools, so the
    /// agent sees only the copy meant for it.
    Imported,
    Unknown,
}

/// `client` is the name the MCP client sent at initialize. `IMBO_HOST`,
/// which Imbo writes into every MCP config, wins; otherwise each tool's
/// marks are asked in order.
pub fn host(client: &str) -> Host {
    let by_name = ALL
        .iter()
        .copied()
        .find(|a| a.mcp_client().is_some_and(|c| client.starts_with(c)));
    if let Some(a) = env("IMBO_HOST").and_then(|h| get(&h)) {
        return match by_name {
            Some(b) if b.id() != a.id() => Host::Imported,
            _ => Host::Tool(a),
        };
    }
    if by_name.is_some_and(|b| b.id() == imbo_proto::TOOL_CURSOR) {
        return Host::Imported;
    }
    match ALL.iter().copied().find(|a| a.owns_mcp(client)) {
        Some(a) => Host::Tool(a),
        None => Host::Unknown,
    }
}

/// The session behind a terminal command as `<tool>:<session>`, from its
/// environment (ADR-008). `Err(tool name)` when the command runs inside a
/// tool's session that can't be told apart (RULE-005).
pub fn terminal_actor() -> Result<Option<String>, &'static str> {
    for a in ALL {
        if let Some(s) = a.session(&[]) {
            return Ok(Some(format!("{}:{s}", a.id())));
        }
        if a.marked() {
            return Err(a.name());
        }
    }
    Ok(None)
}

/// What a tool process tells the daemon about its session (API-LOCAL-001).
pub struct Registration {
    pub session_id: String,
    pub messaging_socket: Option<String>,
    pub messaging_token: Option<String>,
    pub pid: u32,
}

/// A session an adapter knows of.
pub struct Found {
    pub session_id: String,
    /// Empty when the tool has no title for it (yet).
    pub title: String,
    pub online: bool,
}

/// Whether a tool is installed, for `imbo setup` and `imbo uninstall`.
pub enum Install {
    /// Its command or app.
    Found(PathBuf),
    Missing,
    /// Installed, but a version Imbo doesn't work with; the text says why.
    Unsupported(String),
}

/// How an agent hands over pictures pasted into its session (ADR-014). The
/// text goes into the send tool's description.
pub enum Paste {
    /// By number, in send's `images`; the adapter finds the picture.
    Numbered(&'static str),
    /// By the path the tool shows, in send's `files`.
    Path(&'static str),
}

/// What `imbo setup` shows once a tool is connected.
pub struct Connected {
    /// Shown after "<tool> connected".
    pub detail: &'static str,
    /// A step besides the connection itself that failed, and how to do it
    /// by hand.
    pub warning: Option<String>,
}

/// A picture the user pasted into a session (ADR-014).
pub struct PastedImage {
    pub media_type: String,
    pub bytes: Vec<u8>,
}

impl PastedImage {
    pub fn ext(&self) -> &'static str {
        match self.media_type.as_str() {
            "image/jpeg" => "jpg",
            "image/gif" => "gif",
            "image/webp" => "webp",
            _ => "png",
        }
    }
}

pub trait Adapter: Send + Sync {
    /// Tool ID, the part before `:` in an actor.
    fn id(&self) -> &'static str;

    /// Name shown to people.
    fn name(&self) -> &'static str;

    /// Session ID of the caller, from the request's `_meta` objects or the
    /// environment; `None` when the call isn't from this tool.
    fn session(&self, meta: &[&Map<String, Value>]) -> Option<String>;

    /// Start of the client name the tool's MCP client sends at initialize.
    fn mcp_client(&self) -> Option<&'static str> {
        None
    }

    /// Whether this MCP process runs for this tool when `IMBO_HOST` is
    /// missing; `client` is the client name sent at initialize.
    fn owns_mcp(&self, client: &str) -> bool {
        self.mcp_client().is_some_and(|c| client.starts_with(c))
    }

    /// Put before the names of Imbo's tools in this tool, where plain names
    /// like `send` would clash.
    fn tool_prefix(&self) -> &'static str {
        ""
    }

    /// How long the tool waits for one call of an Imbo tool, when that is
    /// too short for a large upload (API-MCP-001).
    fn call_limit(&self) -> Option<Duration> {
        None
    }

    /// The environment says this command runs inside one of this tool's
    /// sessions, even when `session` can't tell which (RULE-005).
    fn marked(&self) -> bool {
        false
    }

    /// Environment the daemon keeps: where the tool stores its files.
    fn env_pass(&self) -> &'static [&'static str];

    /// Environment naming one session, which a daemon must not inherit.
    fn env_session(&self) -> &'static [&'static str];

    /// Registration the tool process repeats; `None` when the tool's
    /// sessions only show up in their calls.
    fn announce(&self) -> Option<Registration> {
        None
    }

    /// Registration the tool process sends before each call it makes for
    /// `session_id`.
    fn announce_call(&self, _session_id: &str) -> Option<Registration> {
        None
    }

    /// API-LOCAL-001 in the daemon; `None` when the session can't be found.
    fn register(&self, r: Registration) -> Option<Found> {
        self.adopt(&r.session_id)
    }

    /// A session that called the daemon before discovery found it; `None`
    /// when the session can't be found.
    fn adopt(&self, session_id: &str) -> Option<Found>;

    /// How often the daemon runs `discover`.
    fn poll_every(&self) -> Duration;

    /// Every session of this tool the daemon should know of, with titles.
    /// Sessions left out count as closed; an error keeps the last list.
    fn discover(&self) -> BoxFuture<'_, Result<Vec<Found>, String>>;

    /// How messages reach the session now, for `imbo status`: `inject`,
    /// `listen`, `prompt_hook` or `queue`.
    fn channel(&self, session_id: &str) -> &'static str;

    /// `Err(reason)` when messages can't be put into the session now; the
    /// delivery goes back to the server with that reason.
    fn ready(&self, _session_id: &str) -> Result<(), &'static str> {
        Ok(())
    }

    /// Messages go to the session's listener (FLOW-LISTEN) instead of
    /// `deliver`.
    fn listens(&self) -> bool {
        false
    }

    /// When a second listener registers for a session, it replaces the one
    /// waiting; otherwise the second one ends at once (API-LOCAL-010).
    fn keep_newest(&self) -> bool {
        false
    }

    /// For `imbo wait` run inside this tool: the session it listens for and
    /// the tool process to outlive no longer than.
    fn wait_target(&self) -> Option<(String, Option<u32>)> {
        None
    }

    /// One of the hooks Imbo installs in this tool (`imbo hook <tool>
    /// <event>`); `input` is what the tool wrote to stdin. Returns what to
    /// print. Never fails: the tool must carry on whatever happens here.
    fn hook<'a>(&'a self, _event: &'a str, _input: &'a Value) -> BoxFuture<'a, Option<String>> {
        Box::pin(std::future::ready(None))
    }

    /// Put a message into the session, written the way this tool shows it.
    fn deliver<'a>(
        &'a self,
        session_id: &'a str,
        m: &'a MessageView,
        files: &'a [LocalFile],
    ) -> BoxFuture<'a, Result<(), String>>;

    fn paste(&self) -> Paste;

    /// Picture `[Image #n]` pasted into the session; the error is for the
    /// agent.
    fn pasted_image(&self, session_id: &str, n: u32) -> Result<PastedImage, String>;

    /// The tool's command or app; `imbo setup` connects only tools found.
    fn find(&self) -> Install;

    /// One line on what connecting changes, shown before it's done
    /// (ADR-013).
    fn changes(&self) -> String;

    /// Connect Imbo and let its tools run without asking each time.
    fn connect(&self, home: &Home, bin: &Path) -> Result<Connected, String>;

    /// Asked in `imbo setup` once the tool is connected, not when `imbo
    /// update` reconnects it; `interactive` when there is a terminal to ask
    /// in. Returns a note to show.
    fn after_connect(&self, _home: &Home, _interactive: bool) -> std::io::Result<Option<String>> {
        Ok(None)
    }

    /// Undo `connect`; `bin` is `None` when the tool isn't installed any more.
    fn disconnect(&self, home: &Home, bin: Option<&Path>);
}

/// `IMBO_FAULT_INJECT=claude-code,codex` (tool IDs) makes those tools'
/// inserts fail (for listening tools, hand-overs), `download` makes
/// attachment downloads fail and `slow-upload` holds each upload 55 seconds
/// (tests of the fallbacks).
pub fn fault(tool: &str) -> bool {
    std::env::var("IMBO_FAULT_INJECT")
        .map(|v| v.split(',').any(|t| t.trim() == tool))
        .unwrap_or(false)
}

/// Run a tool's command; the error is the last line it printed.
fn run(cmd: &Path, args: &[&str]) -> Result<String, String> {
    match Command::new(cmd)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
    {
        Ok(o) if o.status.success() => Ok(String::from_utf8_lossy(&o.stdout).into_owned()),
        Ok(o) => {
            let mut e = String::from_utf8_lossy(&o.stderr).trim().to_string();
            if e.is_empty() {
                e = String::from_utf8_lossy(&o.stdout).trim().to_string();
            }
            Err(e.lines().last().unwrap_or("failed").to_string())
        }
        Err(e) => Err(e.to_string()),
    }
}

/// A non-empty environment variable.
fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|s| !s.is_empty())
}

/// The command on PATH, as `Install`.
fn on_path(name: &str) -> Install {
    match crate::paths::which(name) {
        Some(p) => Install::Found(p),
        None => Install::Missing,
    }
}

/// Where the session each process of `tool` has open is kept, as its hooks
/// last reported it (API-LOCAL-001). On disk, so commands run in that
/// session and a restarted daemon find it.
fn pids_dir(tool: &str) -> PathBuf {
    crate::paths::Home::resolve()
        .state()
        .join("pids")
        .join(tool)
}

/// Note that process `pid` of `tool` has `session_id` open.
fn remember(tool: &str, pid: u32, session_id: &str) {
    let dir = pids_dir(tool);
    if remembered(tool, pid).as_deref() == Some(session_id) {
        return;
    }
    let _ = std::fs::create_dir_all(&dir);
    let tmp = dir.join(format!(".{pid}.tmp"));
    if std::fs::write(&tmp, session_id).is_ok() {
        let _ = std::fs::rename(&tmp, dir.join(pid.to_string()));
    }
}

fn remembered(tool: &str, pid: u32) -> Option<String> {
    std::fs::read_to_string(pids_dir(tool).join(pid.to_string()))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Every running process of `tool` with the session it has open; entries of
/// processes that have exited are removed.
fn remembered_all(tool: &str) -> Vec<(u32, String)> {
    let Ok(dir) = std::fs::read_dir(pids_dir(tool)) else {
        return vec![];
    };
    let mut out = vec![];
    for e in dir.filter_map(|e| e.ok()) {
        let Some(pid) = e.file_name().to_str().and_then(|n| n.parse::<u32>().ok()) else {
            continue;
        };
        if !crate::procs::alive(pid) {
            let _ = std::fs::remove_file(e.path());
            continue;
        }
        if let Some(s) = remembered(tool, pid) {
            out.push((pid, s));
        }
    }
    out
}

/// Process `pid` of `tool` no longer has `session_id` open.
fn forget(tool: &str, pid: u32, session_id: &str) {
    if remembered(tool, pid).as_deref() == Some(session_id) {
        let _ = std::fs::remove_file(pids_dir(tool).join(pid.to_string()));
    }
}

/// API-LOCAL-001 from a hook: `session_id` is open in the tool process
/// `pid`.
async fn hook_register(tool: &str, session_id: &str, pid: u32) {
    let req = LocalRequest::Register {
        tool: tool.into(),
        session_id: session_id.into(),
        messaging_socket: None,
        messaging_token: None,
        pid,
    };
    let _ = Local::new().send_with(&req, Duration::from_secs(1)).await;
}

/// API-LOCAL-003 from a hook: messages sent back to the server, as context
/// for the turn of at most `limit` characters; `None` when there are none
/// or anything fails.
async fn hook_context(tool: &str, session_id: &str, limit: usize) -> Option<String> {
    let req = LocalRequest::HookPrompt {
        tool: tool.into(),
        session_id: session_id.into(),
        limit: Some(limit),
    };
    let res = tokio::time::timeout(
        Duration::from_secs(8),
        Local::new().send_with(&req, Duration::from_secs(1)),
    )
    .await
    .ok()?
    .ok()
    .filter(|r| r.ok)?;
    res.body.get("context")?.as_str().map(str::to_string)
}

/// API-LOCAL-011 from a stop hook: no listener waits for the session, and
/// its listeners haven't been crashing. `false` when anything fails, so the
/// turn ends.
async fn needs_listener(tool: &str, session_id: &str) -> bool {
    let req = LocalRequest::Listening {
        tool: tool.into(),
        session_id: session_id.into(),
    };
    let Ok(res) = Local::new().send_with(&req, Duration::from_secs(1)).await else {
        return false;
    };
    match serde_json::from_value::<ListenerStatus>(res.body) {
        Ok(st) if res.ok => !st.listening && !st.stalled,
        _ => false,
    }
}

/// What the agent writes as the description of the background `imbo wait`
/// (API-LOCAL-011): the wake turn shows it, and it says where the messages
/// are.
const WAKE: &str = "Imbo: new messages. Call the inbox tool to fetch them.";

/// `channel` of a tool that listens: without a listener, messages wait for
/// the next prompt.
fn listen_channel(tool: &str, session_id: &str) -> &'static str {
    if crate::daemon::listen::waiting(tool, session_id) {
        "listen"
    } else {
        "prompt_hook"
    }
}

/// Whether `command`, from a tool's hook config, is one `imbo setup`
/// wrote for `tool` and this Imbo home.
fn our_hook(command: &str, home: &Home, tool: &str) -> bool {
    command.contains(&format!(" hook {tool} "))
        && if home.is_primary() {
            !command.starts_with("IMBO_HOME=")
        } else {
            command.contains(&*home.dir.to_string_lossy())
        }
}

/// The nearest process above this one that `tool` has a session for.
fn ancestor_session(tool: &str) -> Option<(u32, String)> {
    crate::procs::ancestors()
        .into_iter()
        .find_map(|pid| remembered(tool, pid).map(|s| (pid, s)))
}

/// One JSON line to a socket that Imbo's extension or plugin listens on
/// inside a tool, and the one-line answer; `Err` when there is none within
/// `within` or it says `ok: false`.
async fn socket_call(path: &str, req: &Value, within: Duration) -> Result<Value, String> {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    let exchange = async {
        let mut s = tokio::net::UnixStream::connect(path)
            .await
            .map_err(|e| format!("connect {path}: {e}"))?;
        let mut line = serde_json::to_vec(req).unwrap_or_default();
        line.push(b'\n');
        s.write_all(&line)
            .await
            .map_err(|e| format!("write: {e}"))?;
        let mut answer = String::new();
        BufReader::new(s)
            .read_line(&mut answer)
            .await
            .map_err(|e| format!("read: {e}"))?;
        let v: Value = serde_json::from_str(&answer).map_err(|_| "no answer".to_string())?;
        match v["ok"].as_bool() {
            Some(true) => Ok(v),
            _ => Err(v["error"].as_str().unwrap_or("refused").to_string()),
        }
    };
    tokio::time::timeout(within, exchange)
        .await
        .map_err(|_| "timed out".to_string())?
}

/// Another tool's SQLite database, read-only. Its WAL is read too, so rows
/// written a moment ago show; close it soon so the tool can checkpoint.
fn open_ro(path: &Path) -> Option<rusqlite::Connection> {
    use rusqlite::OpenFlags;
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let db = rusqlite::Connection::open_with_flags(path, flags).ok()?;
    db.busy_timeout(Duration::from_millis(500)).ok()?;
    Some(db)
}

/// The nearest process above this one that isn't a shell: the tool that
/// runs this hook or command.
fn tool_process() -> Option<u32> {
    crate::procs::ancestors()
        .into_iter()
        .find(|p| !crate::procs::is_shell(*p))
}

/// A tool's JSON config; a missing or empty file reads as `{}`.
fn read_json(path: &Path) -> Result<Value, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(json!({})),
        Err(e) => return Err(e.to_string()),
    };
    if text.trim().is_empty() {
        return Ok(json!({}));
    }
    let v: Value = serde_json::from_str(&text).map_err(|e| format!("not valid JSON: {e}"))?;
    if !v.is_object() {
        return Err("not a JSON object".into());
    }
    Ok(v)
}

/// Replace a tool's JSON config in one step, so the tool never reads half
/// a file.
fn write_json(path: &Path, v: &Value) -> Result<(), String> {
    let mut text = serde_json::to_string_pretty(v).map_err(|e| e.to_string())?;
    text.push('\n');
    write_text(path, &text)
}

/// Replace a tool's config file in one step.
fn write_text(path: &Path, text: &str) -> Result<(), String> {
    // A config kept elsewhere through a link stays there.
    let real = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let path = real.as_path();
    let dir = path.parent().ok_or("no parent directory")?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("config");
    let tmp = dir.join(format!(".{name}.imbo-tmp"));
    std::fs::write(&tmp, text).map_err(|e| e.to_string())?;
    if let Ok(meta) = std::fs::metadata(path) {
        let _ = std::fs::set_permissions(&tmp, meta.permissions());
    }
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

/// Read `path`, let `edit` change it, and write it back when it changed.
/// When the file can't be read as JSON nothing is written and the error
/// says what to add by hand (FLOW-LOGIN S7).
fn edit_json(
    path: &Path,
    by_hand: &str,
    edit: impl FnOnce(&mut Map<String, Value>) -> Result<(), String>,
) -> Result<(), String> {
    let fail = |e: String| {
        format!(
            "Couldn't edit {} ({e}). Add this yourself: {by_hand}",
            crate::paths::tilde(path)
        )
    };
    let mut v = read_json(path).map_err(fail)?;
    let before = v.clone();
    edit(v.as_object_mut().expect("read_json returns an object")).map_err(fail)?;
    if v != before {
        write_json(path, &v).map_err(fail)?;
    }
    Ok(())
}

/// `parent[key]` as an object, created when missing.
fn object<'a>(
    parent: &'a mut Map<String, Value>,
    key: &str,
) -> Result<&'a mut Map<String, Value>, String> {
    parent
        .entry(key)
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or_else(|| format!("`{key}` isn't an object"))
}

/// Remove `parent[key]` when it is an empty object or list.
fn drop_empty(parent: &mut Map<String, Value>, key: &str) {
    let empty = match parent.get(key) {
        Some(Value::Object(m)) => m.is_empty(),
        Some(Value::Array(a)) => a.is_empty(),
        _ => false,
    };
    if empty {
        parent.remove(key);
    }
}

/// `imbo` as a hook command, with `IMBO_HOME` for a second home (tests).
fn hook_command(home: &crate::paths::Home, args: &str) -> String {
    let exe = sh_quote(&crate::paths::exe().to_string_lossy());
    if home.is_primary() {
        format!("{exe} {args}")
    } else {
        format!(
            "IMBO_HOME={} {exe} {args}",
            sh_quote(&home.dir.to_string_lossy())
        )
    }
}

/// Environment for Imbo's MCP server in a tool's config (ADR-018).
fn server_env(home: &crate::paths::Home, tool: &str) -> Map<String, Value> {
    let mut env = Map::new();
    env.insert("IMBO_HOST".into(), json!(tool));
    if !home.is_primary() {
        env.insert("IMBO_HOME".into(), json!(home.dir.to_string_lossy()));
    }
    env
}

fn sh_quote(s: &str) -> String {
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-+:=@".contains(c))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// A hook's stdin as JSON; `{}` when there is none.
pub fn read_stdin() -> Value {
    use std::io::Read;
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    serde_json::from_str(&input).unwrap_or_else(|_| json!({}))
}
