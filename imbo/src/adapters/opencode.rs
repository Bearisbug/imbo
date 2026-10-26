//! opencode 2: messages go through the prompt endpoint of opencode's shared
//! background service. A busy session takes them at the next step of its
//! turn, an idle one starts a turn (ADR-017).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use base64::Engine;
use futures::future::BoxFuture;
use imbo_proto::api::MessageView;
use imbo_proto::local::LocalFile;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use tracing::{info, warn};

use super::{
    Adapter, Connected, Found, Install, Paste, PastedImage, Registration, drop_empty, edit_json,
    env, object, open_ro, run, server_env,
};
use crate::paths::{Home, exe, tilde, user_home};
use crate::render;

/// A session seen only through its own calls stays online this long, and
/// a delivered message is watched this long.
const CALLED_TTL: Duration = Duration::from_secs(30 * 60);
/// An idle session's message still waiting in its inbox this long after
/// it was posted gets posted again.
const KNOCK_AFTER: Duration = Duration::from_secs(10);

pub static OPENCODE: Opencode = Opencode {
    called: Mutex::new(BTreeMap::new()),
    posted: Mutex::new(Vec::new()),
};

pub struct Opencode {
    /// Root sessions that called Imbo: when they last did, and the opencode
    /// process that ran the call (`None` for terminal commands).
    called: Mutex<BTreeMap<String, (Instant, Option<u32>)>>,
    /// Messages accepted but not yet seen in their session's history.
    posted: Mutex<Vec<Posted>>,
}

struct Posted {
    session_id: String,
    body: Value,
    first: Instant,
    last: Instant,
}

impl Adapter for Opencode {
    fn id(&self) -> &'static str {
        imbo_proto::TOOL_OPENCODE
    }

    fn name(&self) -> &'static str {
        "opencode"
    }

    /// Each call's `_meta` names the session; shell commands get
    /// `OPENCODE_SESSION_ID`. A subagent's session counts as its root
    /// session, the one people see.
    fn session(&self, meta: &[&Map<String, Value>]) -> Option<String> {
        let id = meta
            .iter()
            .find_map(|m| m.get("ai.opencode/sessionID")?.as_str())
            .map(str::to_string)
            .or_else(|| env("OPENCODE_SESSION_ID"))?;
        Some(root(&id))
    }

    fn mcp_client(&self) -> Option<&'static str> {
        Some("opencode")
    }

    /// opencode 1 sets no session variable for shell commands.
    fn marked(&self) -> bool {
        env("AI_AGENT").as_deref() == Some("opencode")
    }

    fn env_pass(&self) -> &'static [&'static str] {
        &["XDG_STATE_HOME", "XDG_DATA_HOME", "OPENCODE_DB"]
    }

    fn env_session(&self) -> &'static [&'static str] {
        &["OPENCODE_SESSION_ID"]
    }

    /// One MCP process serves every session in a directory, so it tells the
    /// daemon the session with each call. Its parent is the opencode process
    /// that runs the session.
    fn announce_call(&self, session_id: &str) -> Option<Registration> {
        Some(Registration {
            session_id: session_id.into(),
            messaging_socket: None,
            messaging_token: None,
            pid: unsafe { libc::getppid() } as u32,
        })
    }

    fn register(&self, r: Registration) -> Option<Found> {
        let mut called = self.called.lock().unwrap();
        called.insert(r.session_id.clone(), (Instant::now(), Some(r.pid)));
        Some(Found {
            session_id: r.session_id,
            title: String::new(),
            online: true,
        })
    }

    fn adopt(&self, session_id: &str) -> Option<Found> {
        let mut called = self.called.lock().unwrap();
        let pid = called.get(session_id).and_then(|(_, p)| *p);
        called.insert(session_id.into(), (Instant::now(), pid));
        Some(Found {
            session_id: session_id.into(),
            title: String::new(),
            online: true,
        })
    }

    fn poll_every(&self) -> Duration {
        Duration::from_secs(5)
    }

    /// Sessions in the TUI's tabs, the busy ones, and those that called
    /// within `CALLED_TTL`, as the service knows them. Archived and deleted
    /// ones are left out. When the service isn't running, only the sessions
    /// that called.
    fn discover(&self) -> BoxFuture<'_, Result<Vec<Found>, String>> {
        Box::pin(async move {
            let called: Vec<String> = {
                let mut c = self.called.lock().unwrap();
                c.retain(|_, (t, _)| t.elapsed() < CALLED_TTL);
                c.keys().cloned().collect()
            };
            if service_of(None).is_none() {
                return Ok(called
                    .into_iter()
                    .map(|session_id| Found {
                        session_id,
                        title: String::new(),
                        online: true,
                    })
                    .collect());
            }
            let mut ids: BTreeSet<String> = tabs().into_iter().collect();
            ids.extend(called);
            let mut busy = BTreeSet::new();
            if let Ok((200, v)) = api(None, "GET", "/api/session/active", None).await
                && let Some(active) = v.as_object()
            {
                busy.extend(active.keys().cloned());
            }
            self.recheck(&busy).await;
            ids.extend(busy);
            let mut seen = BTreeSet::new();
            let mut list = vec![];
            for id in ids {
                let Some(s) = root_info(&id).await? else {
                    continue;
                };
                if !s.archived && seen.insert(s.id.clone()) {
                    list.push(Found {
                        session_id: s.id,
                        title: s.title,
                        online: true,
                    });
                }
            }
            Ok(list)
        })
    }

    /// `queue` while no service runs, or when the session's calls come
    /// from an opencode process of its own (`--standalone`), whose window
    /// wouldn't show what a service runs.
    fn channel(&self, session_id: &str) -> &'static str {
        match service_of(Some(session_id)) {
            Some(_) => "inject",
            None => "queue",
        }
    }

    fn ready(&self, session_id: &str) -> Result<(), &'static str> {
        match self.channel(session_id) {
            "inject" => Ok(()),
            _ => Err("not-on-shared-service"),
        }
    }

    fn deliver<'a>(
        &'a self,
        session_id: &'a str,
        m: &'a MessageView,
        files: &'a [LocalFile],
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let Some(s) = info(session_id).await? else {
                self.called.lock().unwrap().remove(session_id);
                return Err("opencode has no such session".into());
            };
            if s.archived {
                return Err("the session is archived".into());
            }
            // A prompt would make the staged undo final.
            if s.revert {
                return Err("the session has an undo staged".into());
            }
            let body = json!({
                "id": message_id(m, session_id),
                "text": render::for_codex(m, files),
                "metadata": { "imbo": { "message_id": m.id } },
            });
            let path = format!("/api/session/{session_id}/prompt");
            match api(Some(session_id), "POST", &path, Some(&body)).await? {
                // 409: this ID was taken by the same message before.
                (200..=299 | 409, _) => {
                    info!(msg = m.id, session = %session_id, "delivered into opencode session");
                    self.posted.lock().unwrap().push(Posted {
                        session_id: session_id.into(),
                        body,
                        first: Instant::now(),
                        last: Instant::now(),
                    });
                    Ok(())
                }
                (status, v) => {
                    warn!(msg = m.id, session = %session_id, status, "opencode refused the message");
                    Err(format!("opencode answered HTTP {status}: {}", short(&v)))
                }
            }
        })
    }

    fn paste(&self) -> Paste {
        Paste::Numbered(
            " Images the user pasted in this session ([Image N]): put the numbers in images to send them as attachments; N refers to the user's latest message with that number.",
        )
    }

    fn pasted_image(&self, session_id: &str, n: u32) -> Result<PastedImage, String> {
        stored_image(session_id, n).ok_or_else(|| {
            format!("Couldn't find the pasted image [Image {n}] in the session log; ask the user to paste it again, then send.")
        })
    }

    /// `opencode2`, or `opencode` when it is opencode 2; the install script
    /// puts both in `~/.opencode/bin`, which may not be on this PATH.
    fn find(&self) -> Install {
        let own = user_home().join(".opencode/bin");
        let at = |name: &str| {
            crate::paths::which(name).or_else(|| {
                let p = own.join(name);
                p.is_file().then_some(p)
            })
        };
        if let Some(p) = at("opencode2") {
            return Install::Found(p);
        }
        let Some(p) = at("opencode") else {
            return Install::Missing;
        };
        let version = run(&p, &["--version"]).unwrap_or_default();
        match major(&version) {
            Some(v) if v >= 2 => Install::Found(p),
            _ => Install::Unsupported(format!(
                "opencode {} isn't supported; Imbo works with opencode 2",
                version.trim()
            )),
        }
    }

    fn changes(&self) -> String {
        format!(
            "opencode  {}  mcp.servers.imbo (codemode off); \"imbo_*\": \"allow\" in permission",
            tilde(&config_file())
        )
    }

    /// opencode picks up config changes by itself.
    fn connect(&self, home: &Home, _bin: &Path) -> Result<Connected, String> {
        let server = json!({
            "type": "local",
            "command": [exe().to_string_lossy(), "mcp"],
            "codemode": false,
            "environment": server_env(home, self.id()),
        });
        let by_hand = format!(
            "\"mcp\": {{\"servers\": {{\"imbo\": {server}}}}}, \"permission\": {{\"imbo_*\": \"allow\"}}"
        );
        edit_json(&config_file(), &by_hand, |root| {
            object(object(root, "mcp")?, "servers")?.insert("imbo".into(), server.clone());
            allow(root)
        })?;
        Ok(Connected {
            detail: "MCP server imbo",
            warning: None,
        })
    }

    fn disconnect(&self, _home: &Home, _bin: Option<&Path>) {
        let _ = edit_json(&config_file(), "", |root| {
            if let Some(Value::Object(mcp)) = root.get_mut("mcp") {
                mcp.remove("imbo");
                if let Some(Value::Object(servers)) = mcp.get_mut("servers") {
                    servers.remove("imbo");
                }
                drop_empty(mcp, "servers");
            }
            drop_empty(root, "mcp");
            if let Some(Value::Object(p)) = root.get_mut("permission") {
                p.remove(RULE);
            }
            drop_empty(root, "permission");
            if let Some(Value::Array(rules)) = root.get_mut("permissions") {
                rules.retain(|r| r.get("action").and_then(Value::as_str) != Some(RULE));
            }
            drop_empty(root, "permissions");
            Ok(())
        });
    }
}

impl Opencode {
    /// A posted message waits in the session's inbox until a turn takes it
    /// into the history. A turn stopped without resuming, or a service
    /// restart before the turn began, leaves it waiting: once the session is
    /// idle it is posted again with the same ID, which adds nothing and only
    /// starts a turn. Gone from both means the user took it back.
    async fn recheck(&self, busy: &BTreeSet<String>) {
        let list = std::mem::take(&mut *self.posted.lock().unwrap());
        let mut keep = vec![];
        for mut p in list {
            if p.first.elapsed() > CALLED_TTL {
                continue;
            }
            let sid = p.session_id.clone();
            let id = p.body["id"].as_str().unwrap_or_default().to_string();
            match api(
                Some(&sid),
                "GET",
                &format!("/api/session/{sid}/message/{id}"),
                None,
            )
            .await
            {
                Ok((200, _)) => continue,
                Ok((404, _)) => {}
                _ => {
                    keep.push(p);
                    continue;
                }
            }
            match api(
                Some(&sid),
                "GET",
                &format!("/api/session/{sid}/inbox"),
                None,
            )
            .await
            {
                Ok((200, v)) if v.to_string().contains(&id) => {}
                Ok((200 | 404, _)) => continue,
                _ => {
                    keep.push(p);
                    continue;
                }
            }
            if !busy.contains(&sid) && p.last.elapsed() >= KNOCK_AFTER {
                info!(session = %sid, "message still waits in the opencode inbox; posting it again");
                let path = format!("/api/session/{sid}/prompt");
                let _ = api(Some(&sid), "POST", &path, Some(&p.body)).await;
                p.last = Instant::now();
            }
            keep.push(p);
        }
        self.posted.lock().unwrap().extend(keep);
    }
}

/// Imbo's tools as opencode's permission rules name them.
const RULE: &str = "imbo_*";

/// Let Imbo's tools run without asking: the last matching rule wins, so the
/// rule goes last. opencode 2's `permissions` list is used when the user
/// already has one; opencode 1 can't read that list and shares the file.
fn allow(root: &mut Map<String, Value>) -> Result<(), String> {
    if let Some(Value::Array(rules)) = root.get_mut("permissions") {
        rules.retain(|r| r.get("action").and_then(Value::as_str) != Some(RULE));
        rules.push(json!({ "action": RULE, "resource": "*", "effect": "allow" }));
        return Ok(());
    }
    match root.get_mut("permission") {
        // A single word applies to every tool.
        Some(Value::String(_)) => Ok(()),
        Some(Value::Object(p)) => {
            p.remove(RULE);
            p.insert(RULE.into(), json!("allow"));
            Ok(())
        }
        Some(_) => Err("`permission` isn't an object".into()),
        None => {
            root.insert("permission".into(), json!({ RULE: "allow" }));
            Ok(())
        }
    }
}

/// `opencode v2.1.4` → 2, `1.19.4` → 1.
fn major(version: &str) -> Option<u32> {
    let v = version.trim();
    let v = v.strip_prefix("opencode").unwrap_or(v).trim();
    let v = v.strip_prefix('v').unwrap_or(v);
    v.split('.').next()?.parse().ok()
}

fn config_dir() -> PathBuf {
    if let Some(d) = env("OPENCODE_CONFIG_DIR") {
        return PathBuf::from(d);
    }
    env("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| user_home().join(".config"))
        .join("opencode")
}

/// `opencode.jsonc` when it exists (it overrides `opencode.json`, and is
/// the one opencode itself edits), else `opencode.json`.
fn config_file() -> PathBuf {
    let dir = config_dir();
    let jsonc = dir.join("opencode.jsonc");
    if jsonc.exists() {
        jsonc
    } else {
        dir.join("opencode.json")
    }
}

fn state_dir() -> PathBuf {
    env("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| user_home().join(".local/state"))
        .join("opencode")
}

fn db_path() -> PathBuf {
    env("OPENCODE_DB").map(PathBuf::from).unwrap_or_else(|| {
        env("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| user_home().join(".local/share"))
            .join("opencode/opencode.db")
    })
}

/// The registration of opencode's shared background service.
#[derive(Deserialize)]
struct Service {
    url: String,
    pid: u32,
    #[serde(default)]
    password: Option<String>,
}

/// The shared services that run, the one of release builds
/// (`service.json`) first; other build channels register as
/// `service-<channel>.json`. A service started with `--standalone` or
/// `opencode serve` in a terminal doesn't register. A killed service leaves
/// its file behind.
fn services() -> Vec<Service> {
    let Ok(dir) = std::fs::read_dir(state_dir()) else {
        return vec![];
    };
    let mut files: Vec<PathBuf> = dir
        .filter_map(|e| Some(e.ok()?.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("service") && n.ends_with(".json"))
        })
        .collect();
    files.sort_by_key(|p| !p.ends_with("service.json"));
    files
        .iter()
        .filter_map(|p| serde_json::from_str::<Service>(&std::fs::read_to_string(p).ok()?).ok())
        .filter(|s| crate::procs::alive(s.pid))
        .collect()
}

/// The service a session runs on: the one whose process ran its calls,
/// else the first. `None` when no service runs, or when the calls came from
/// an opencode process that isn't one.
fn service_of(session_id: Option<&str>) -> Option<Service> {
    let pid = session_id.and_then(|s| OPENCODE.called.lock().unwrap().get(s)?.1);
    let mut all = services().into_iter();
    match pid {
        Some(pid) => all.find(|s| s.pid == pid),
        None => all.next(),
    }
}

/// A call to the API of the service `session_id` runs on; the body is what
/// its `data` holds. A rejected password is tried once more with the
/// registration read again, in case a newer service replaced the one read.
async fn api(
    session_id: Option<&str>,
    method: &str,
    path: &str,
    body: Option<&Value>,
) -> Result<(u16, Value), String> {
    let mut why = String::new();
    for _ in 0..2 {
        let Some(s) = service_of(session_id) else {
            return Err("opencode's background service isn't running".into());
        };
        let auth = s.password.as_ref().map(|p| {
            let pair = format!("opencode:{p}");
            format!(
                "Basic {}",
                base64::engine::general_purpose::STANDARD.encode(pair)
            )
        });
        match crate::net::local_http(&s.url, method, path, auth.as_deref(), body).await {
            Ok((401, _)) => why = "opencode's service refused the password".into(),
            Ok((status, mut v)) => {
                let data = v.get_mut("data").map(Value::take);
                return Ok((status, data.unwrap_or(v)));
            }
            Err(e) => why = e,
        }
    }
    Err(why)
}

/// What Imbo needs of a session.
struct Info {
    id: String,
    parent: Option<String>,
    title: String,
    archived: bool,
    revert: bool,
}

/// `None` when the service has no such session.
async fn info(session_id: &str) -> Result<Option<Info>, String> {
    match api(
        Some(session_id),
        "GET",
        &format!("/api/session/{session_id}"),
        None,
    )
    .await?
    {
        (404, _) => Ok(None),
        (200, v) => Ok(Some(Info {
            id: v["id"].as_str().unwrap_or(session_id).to_string(),
            parent: v["parentID"].as_str().map(str::to_string),
            title: v["title"].as_str().unwrap_or_default().to_string(),
            archived: v["time"]["archived"].is_number(),
            revert: !v["revert"].is_null(),
        })),
        (status, v) => Err(format!("opencode answered HTTP {status}: {}", short(&v))),
    }
}

/// The root session above `session_id`, as the service knows it.
async fn root_info(session_id: &str) -> Result<Option<Info>, String> {
    let mut id = session_id.to_string();
    for _ in 0..8 {
        let Some(s) = info(&id).await? else {
            return Ok(None);
        };
        match &s.parent {
            Some(p) => id = p.clone(),
            None => return Ok(Some(s)),
        }
    }
    Ok(None)
}

/// The root session above `session_id`, from opencode's database; the
/// session itself when the database can't say. Roots never change, so
/// they are kept.
fn root(session_id: &str) -> String {
    static ROOTS: LazyLock<Mutex<HashMap<String, String>>> = LazyLock::new(Default::default);
    if let Some(r) = ROOTS.lock().unwrap().get(session_id) {
        return r.clone();
    }
    let found = (|| {
        let db = open_ro(&db_path())?;
        let mut id = session_id.to_string();
        for _ in 0..8 {
            let parent: Option<String> = db
                .query_row(
                    "SELECT parent_id FROM session_v2 WHERE id = ?1",
                    [&id],
                    |r| r.get(0),
                )
                .ok()?;
            match parent.filter(|p| !p.is_empty()) {
                Some(p) => id = p,
                None => return Some(id),
            }
        }
        None
    })();
    match found {
        Some(r) => {
            ROOTS.lock().unwrap().insert(session_id.into(), r.clone());
            r
        }
        None => session_id.to_string(),
    }
}

/// Sessions in the tabs of the TUIs that run. Each build channel keeps its
/// tabs in `<channel>/tui/tabs.json`, grouped by the TUI's working
/// directory (or one `global` group); a closed TUI leaves its tabs behind,
/// so only the groups of running TUIs count. Tabs may name deleted
/// sessions; discovery checks each with the service.
fn tabs() -> Vec<String> {
    let services: Vec<u32> = services().iter().map(|s| s.pid).collect();
    let dirs: BTreeSet<String> = crate::procs::pids()
        .into_iter()
        .filter(|p| !services.contains(p))
        .filter(|p| crate::procs::name(*p).is_some_and(|n| n == "opencode" || n == "opencode2"))
        .filter_map(|p| Some(crate::procs::cwd(p)?.to_string_lossy().into_owned()))
        .collect();
    if dirs.is_empty() {
        return vec![];
    }
    let Ok(channels) = std::fs::read_dir(state_dir()) else {
        return vec![];
    };
    let mut ids = vec![];
    for c in channels.flatten() {
        let Ok(text) = std::fs::read_to_string(c.path().join("tui/tabs.json")) else {
            continue;
        };
        let v: Value = serde_json::from_str(&text).unwrap_or_default();
        let mut groups: Vec<&Value> = vec![&v["global"]];
        if let Some(by_dir) = v["cwd"].as_object() {
            groups.extend(
                by_dir
                    .iter()
                    .filter(|(d, _)| dirs.contains(*d))
                    .map(|(_, g)| g),
            );
        }
        ids.extend(
            groups
                .into_iter()
                .filter_map(|g| g["tabs"].as_array())
                .flatten()
                .filter_map(|t| t["sessionID"].as_str().map(str::to_string)),
        );
    }
    ids
}

/// The same opencode message ID for every try of the same Imbo message in
/// the same session, so a retry after a lost answer doesn't add it twice.
/// IDs are unique across opencode's database, so a group message for two
/// sessions gets two. opencode's IDs are 12 hex digits of time and 14
/// base-62 characters.
fn message_id(m: &MessageView, session_id: &str) -> String {
    const CHARS: &[u8] = b"0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
    let time = (m.created_at.timestamp_millis().max(0) as u64) * 0x1000 + 1;
    let hash = Sha256::digest(format!("imbo-message-{}-{session_id}", m.id));
    let tail: String = hash[..14]
        .iter()
        .map(|b| CHARS[*b as usize % 62] as char)
        .collect();
    format!("msg_{:012x}{tail}", time & 0xffff_ffff_ffff)
}

/// Picture `[Image N]` from the user's latest message that has one, read
/// from opencode's database.
fn stored_image(session_id: &str, n: u32) -> Option<PastedImage> {
    let db = open_ro(&db_path())?;
    let mut q = db
        .prepare(
            "SELECT data FROM session_message WHERE session_id = ?1 AND type = 'user' \
             ORDER BY seq DESC LIMIT 200",
        )
        .ok()?;
    let rows = q.query_map([session_id], |r| r.get::<_, String>(0)).ok()?;
    let label = format!("[Image {n}]");
    for data in rows.flatten() {
        let v: Value = serde_json::from_str(&data).unwrap_or_default();
        let Some(file) = v["files"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|f| f["mention"]["text"].as_str() == Some(&label))
        else {
            continue;
        };
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(file["data"].as_str()?)
            .ok()?;
        return Some(PastedImage {
            media_type: file["mime"].as_str().unwrap_or("image/png").to_string(),
            bytes,
        });
    }
    None
}

/// The start of an error body, for logs and nack reasons.
fn short(v: &Value) -> String {
    let s = v.to_string();
    s.chars().take(200).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_major_reads_both_styles() {
        assert_eq!(major("opencode v2.1.4\n"), Some(2));
        assert_eq!(major("1.19.4"), Some(1));
        assert_eq!(major(""), None);
    }

    #[test]
    fn allow_rule_goes_last_in_either_shape() {
        let mut root = serde_json::from_str::<Map<String, Value>>(
            r#"{"permission":{"imbo_*":"allow","bash":"ask"}}"#,
        )
        .unwrap();
        allow(&mut root).unwrap();
        let keys: Vec<&String> = root["permission"].as_object().unwrap().keys().collect();
        assert_eq!(keys, ["bash", "imbo_*"]);

        let mut root = serde_json::from_str::<Map<String, Value>>(
            r#"{"permissions":[{"action":"*","resource":"*","effect":"ask"}]}"#,
        )
        .unwrap();
        allow(&mut root).unwrap();
        allow(&mut root).unwrap();
        let rules = root["permissions"].as_array().unwrap();
        assert_eq!(rules.len(), 2);
        assert_eq!(rules[1]["action"], "imbo_*");
        assert!(!root.contains_key("permission"));
    }

    #[test]
    fn message_ids_are_stable_and_well_formed() {
        let m: MessageView = serde_json::from_value(json!({
            "id": 42,
            "from": { "account_id": 1, "name": "alice" },
            "conversation": { "kind": "direct", "id": 7 },
            "body": "hi",
            "created_at": "2026-10-26T00:00:00Z",
            "reply_to": "alice"
        }))
        .unwrap();
        let a = message_id(&m, "ses_a");
        assert_eq!(a, message_id(&m, "ses_a"));
        assert_ne!(a, message_id(&m, "ses_b"));
        assert!(a.starts_with("msg_"));
        assert_eq!(a.len(), 4 + 12 + 14);
    }
}
