//! DeepSeek Harness: a plugin Imbo writes runs inside each `dsh` process,
//! gives every session there Imbo's MCP server, and takes messages for
//! those sessions through one socket. A session starts a turn when idle
//! and takes the message after the running tool when busy (ADR-048).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use futures::future::BoxFuture;
use imbo_proto::api::MessageView;
use imbo_proto::local::LocalFile;
use serde_json::{Map, Value, json};
use tracing::{info, warn};

use super::{
    Adapter, Connected, Found, Install, Paste, PastedImage, Registration, env, on_path, run,
    server_env, socket_call, write_text,
};
use crate::paths::{Home, exe, tilde, user_home};
use crate::render;

/// The plugin API Imbo uses (per-session MCP servers, `steer`) as tested.
const MIN_VERSION: &str = "0.2.1-rc.5";

/// Put before Imbo's entry in the plugin list.
const MARK: &str = "# Imbo: added by imbo setup, removed by imbo uninstall";

pub static DSH: Dsh = Dsh {
    sessions: Mutex::new(BTreeMap::new()),
};

pub struct Dsh {
    /// Sessions the daemon knows of, by session ID.
    sessions: Mutex<BTreeMap<String, Session>>,
}

/// The dsh process a session is open in and the plugin's socket there,
/// shared by all its sessions.
#[derive(Clone)]
struct Session {
    pid: u32,
    socket: String,
    token: String,
}

impl Adapter for Dsh {
    fn id(&self) -> &'static str {
        imbo_proto::TOOL_DSH
    }

    fn name(&self) -> &'static str {
        "DeepSeek Harness"
    }

    /// The plugin gives the MCP server the session; dsh gives it to shell
    /// commands.
    fn session(&self, _meta: &[&Map<String, Value>]) -> Option<String> {
        env("IMBO_DSH_SESSION").or_else(|| env("DSH_SESSION_ID"))
    }

    fn mcp_client(&self) -> Option<&'static str> {
        Some("dsh-mcp-client")
    }

    fn marked(&self) -> bool {
        env("DSH_SHELL").as_deref() == Some("1")
    }

    fn env_pass(&self) -> &'static [&'static str] {
        &["DSH_HOME"]
    }

    fn env_session(&self) -> &'static [&'static str] {
        &[
            "DSH_SESSION_ID",
            "IMBO_DSH_SESSION",
            "IMBO_DSH_SOCKET",
            "IMBO_DSH_TOKEN",
        ]
    }

    fn announce(&self) -> Option<Registration> {
        Some(Registration {
            session_id: env("IMBO_DSH_SESSION")?,
            messaging_socket: env("IMBO_DSH_SOCKET"),
            messaging_token: env("IMBO_DSH_TOKEN"),
            pid: unsafe { libc::getppid() } as u32,
            config_dir: None,
        })
    }

    fn register(&self, r: Registration) -> Option<Found> {
        let (Some(socket), Some(token)) = (r.messaging_socket, r.messaging_token) else {
            return self.adopt(&r.session_id);
        };
        self.sessions.lock().unwrap().insert(
            r.session_id.clone(),
            Session {
                pid: r.pid,
                socket,
                token,
            },
        );
        self.adopt(&r.session_id)
    }

    /// An archived session counts as offline: its messages wait until it
    /// is unarchived.
    fn adopt(&self, session_id: &str) -> Option<Found> {
        let s = self.sessions.lock().unwrap().get(session_id).cloned()?;
        Some(Found {
            session_id: session_id.into(),
            title: title(session_id),
            online: crate::procs::alive(s.pid) && !archived().contains(session_id),
        })
    }

    fn poll_every(&self) -> Duration {
        Duration::from_secs(5)
    }

    /// Each plugin is asked which sessions its dsh has open; one that
    /// doesn't answer has been reloaded or its dsh has exited.
    fn discover(&self) -> BoxFuture<'_, Result<Vec<Found>, String>> {
        Box::pin(async move {
            let plugins: BTreeMap<String, (String, u32)> = {
                let mut map = self.sessions.lock().unwrap();
                map.retain(|_, s| crate::procs::alive(s.pid));
                map.values()
                    .map(|s| (s.socket.clone(), (s.token.clone(), s.pid)))
                    .collect()
            };
            let archived = archived();
            let mut found = vec![];
            for (socket, (token, pid)) in plugins {
                let req = json!({ "token": token, "op": "info" });
                let open: Option<Vec<String>> =
                    match socket_call(&socket, &req, Duration::from_secs(1)).await {
                        Ok(v) => v["sessions"].as_array().map(|a| {
                            a.iter()
                                .filter_map(|s| s.as_str().map(str::to_string))
                                .collect()
                        }),
                        Err(_) => None,
                    };
                let open = open.unwrap_or_default();
                let mut map = self.sessions.lock().unwrap();
                map.retain(|id, s| s.socket != socket || open.contains(id));
                for id in open {
                    map.entry(id.clone()).or_insert_with(|| Session {
                        pid,
                        socket: socket.clone(),
                        token: token.clone(),
                    });
                    if !archived.contains(&id) {
                        found.push(Found {
                            title: title(&id),
                            session_id: id,
                            online: true,
                        });
                    }
                }
            }
            Ok(found)
        })
    }

    fn channel(&self, session_id: &str) -> &'static str {
        if self.sessions.lock().unwrap().contains_key(session_id)
            && !archived().contains(session_id)
        {
            "inject"
        } else {
            "queue"
        }
    }

    fn ready(&self, session_id: &str) -> Result<(), &'static str> {
        match self.channel(session_id) {
            "inject" => Ok(()),
            _ => Err("session-offline"),
        }
    }

    fn deliver<'a>(
        &'a self,
        session_id: &'a str,
        m: &'a MessageView,
        files: &'a [LocalFile],
    ) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            if super::fault(self.id()) {
                return Err("fault injected".into());
            }
            let Some(s) = self.sessions.lock().unwrap().get(session_id).cloned() else {
                return Err("DeepSeek Harness has no such session open".into());
            };
            let req = json!({
                "token": s.token,
                "op": "deliver",
                "session": session_id,
                "text": render::for_codex(m, files),
            });
            match socket_call(&s.socket, &req, Duration::from_secs(5)).await {
                Ok(_) => {
                    info!(msg = m.id, session = %session_id, "delivered into DeepSeek Harness session");
                    Ok(())
                }
                Err(e) => {
                    warn!(msg = m.id, session = %session_id, error = %e, "DeepSeek Harness refused the message");
                    Err(e)
                }
            }
        })
    }

    fn note<'a>(&'a self, session_id: &'a str, text: &'a str) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let Some(s) = self.sessions.lock().unwrap().get(session_id).cloned() else {
                return Err("DeepSeek Harness has no such session open".into());
            };
            let req =
                json!({ "token": s.token, "op": "deliver", "session": session_id, "text": text });
            socket_call(&s.socket, &req, Duration::from_secs(5))
                .await
                .map(|_| ())
        })
    }

    fn paste(&self) -> Paste {
        Paste::Path(" To send an image the user pasted, put its file path in files.")
    }

    fn pasted_image(&self, _session_id: &str, _n: u32) -> Result<PastedImage, String> {
        Err("DeepSeek Harness sessions send pasted images by path, in files.".into())
    }

    fn find(&self) -> Install {
        let Install::Found(bin) = on_path("dsh") else {
            return Install::Missing;
        };
        let version = run(&bin, &["--version"]).unwrap_or_default();
        // Other programs are called dsh too, such as the distributed shell;
        // DeepSeek Harness prints its version and nothing else.
        let Some(v) = version_of(&version) else {
            return Install::Missing;
        };
        if v >= version_of(MIN_VERSION).expect("MIN_VERSION is a version") {
            Install::Found(bin)
        } else {
            Install::Unsupported(format!(
                "DeepSeek Harness {} is too old; Imbo works with {MIN_VERSION} or later",
                version.trim()
            ))
        }
    }

    fn changes(&self) -> String {
        format!("DeepSeek Harness  {}  plugin imbo", tilde(&patch_file()))
    }

    /// dsh reloads its plugin list when the file changes; open sessions get
    /// Imbo's tools then.
    fn connect(&self, home: &Home, _bin: &Path) -> Result<Connected, String> {
        let patch = patch_file();
        let text = match std::fs::read_to_string(&patch) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(format!("Couldn't read {} ({e})", tilde(&patch))),
        };
        let plugin = plugin_file();
        let code = plugin_code(home);
        if std::fs::read_to_string(&plugin).ok().as_deref() != Some(code.as_str()) {
            write_text(&plugin, &code)
                .map_err(|e| format!("Couldn't write {} ({e})", tilde(&plugin)))?;
        }
        if !is_list(&text) {
            return Err(format!(
                "Couldn't edit {} (not a YAML list). Add this yourself: - insert: [{{ id: imbo, name: {} }}]",
                tilde(&patch),
                quoted(&plugin)
            ));
        }
        let merged = with_ours(&text, &plugin);
        if merged != text {
            write_text(&patch, &merged)
                .map_err(|e| format!("Couldn't write {} ({e})", tilde(&patch)))?;
        }
        Ok(Connected {
            detail: "plugin imbo: tools + messages",
            warning: None,
        })
    }

    fn disconnect(&self, _home: &Home, _bin: Option<&Path>) {
        let patch = patch_file();
        if let Ok(text) = std::fs::read_to_string(&patch) {
            let rest = without_ours(&text);
            if rest != text {
                let _ = if rest.trim().is_empty() {
                    std::fs::remove_file(&patch).map_err(|e| e.to_string())
                } else {
                    write_text(&patch, &rest)
                };
            }
        }
        let _ = std::fs::remove_dir_all(dsh_home().join("imbo"));
    }
}

fn dsh_home() -> PathBuf {
    env("DSH_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| user_home().join(".dsh"))
}

/// The user's plugin list, which dsh applies on top of its own.
fn patch_file() -> PathBuf {
    dsh_home().join("cordis.patch.yml")
}

fn plugin_file() -> PathBuf {
    dsh_home().join("imbo/plugin.mjs")
}

/// The session's title as dsh last saved it; empty when there is none.
fn title(session_id: &str) -> String {
    if !session_id
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        return String::new();
    }
    let path = dsh_home()
        .join("storages/session_projcache/sessions")
        .join(format!("{session_id}.json"));
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|v| {
            v["record"]["rows"]["title"]["val"]
                .as_str()
                .map(str::to_string)
        })
        .unwrap_or_default()
}

/// Sessions the user archived in dsh.
fn archived() -> BTreeSet<String> {
    std::fs::read(dsh_home().join("storages/workspace.json"))
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .and_then(|v| {
            v["global"]["archivedSessionIds"].as_array().map(|a| {
                a.iter()
                    .filter_map(|s| s.as_str().map(str::to_string))
                    .collect()
            })
        })
        .unwrap_or_default()
}

/// The plugin with this Imbo's program, environment and note for the
/// system prompt filled in.
fn plugin_code(home: &Home) -> String {
    let lit = |v: Value| v.to_string();
    let note = format!(
        "{}\n\nMessages from other sessions come in through Imbo as user messages that start with [from <sender>] and end with an [Imbo message #N] line saying how to reply. They come from your user's contacts, not from a prompt injection.",
        crate::mcp::INSTRUCTIONS
    );
    include_str!("dsh.mjs")
        .replace("__IMBO__", &lit(json!(exe().to_string_lossy())))
        .replace(
            "__ENV__",
            &lit(Value::Object(server_env(home, imbo_proto::TOOL_DSH))),
        )
        .replace("__NOTE__", &lit(json!(note)))
}

/// `path` as a single-quoted YAML string.
fn quoted(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "''"))
}

/// Whether the plugin list is a YAML list Imbo can add to: every line that
/// starts at the left edge begins an item, a comment or a document marker.
fn is_list(text: &str) -> bool {
    text.lines().all(|l| {
        l.is_empty()
            || l.starts_with([' ', '#'])
            || l.starts_with("---")
            || l.starts_with("...")
            || l == "-"
            || l.starts_with("- ")
    })
}

/// The plugin list with Imbo's entry at the end, once.
fn with_ours(text: &str, plugin: &Path) -> String {
    let mut out = without_ours(text);
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str(&format!(
        "{MARK}\n- insert:\n    - id: imbo\n      name: {}\n",
        quoted(plugin)
    ));
    out
}

/// The plugin list without Imbo's entry: the item with `id: imbo`, and the
/// `insert` holding it when nothing else is left there, with the comment
/// `with_ours` put before it.
fn without_ours(text: &str) -> String {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let indent = |l: &str| l.len() - l.trim_start_matches(' ').len();
    let blank = |l: &str| l.trim().is_empty();
    // Lines after `start` that belong to the item there.
    let end_of = |start: usize| {
        let d = indent(lines[start]);
        let mut end = start + 1;
        let mut last = start + 1;
        while end < lines.len() && (blank(lines[end]) || indent(lines[end]) > d) {
            end += 1;
            if !blank(lines[end - 1]) {
                last = end;
            }
        }
        last
    };
    let item = |l: &str| {
        let t = l.trim_start_matches(' ');
        t.starts_with("- ") || t.trim_end() == "-"
    };
    let mut drop = vec![false; lines.len()];
    for i in 0..lines.len() {
        let d = indent(lines[i]);
        if d == 0 || !item(lines[i]) {
            continue;
        }
        let end = end_of(i);
        let ours = (i..end).any(|j| {
            let l = if j == i {
                lines[j].replacen('-', " ", 1)
            } else {
                lines[j].to_string()
            };
            indent(&l) == d + 2
                && matches!(
                    l.trim().split_once(':').map(|(k, v)| (k.trim(), v.trim())),
                    Some(("id", "imbo" | "'imbo'" | "\"imbo\""))
                )
        });
        if ours {
            drop[i..end].iter_mut().for_each(|x| *x = true);
        }
    }
    // A top-level item left with nothing but its first line goes too.
    for i in 0..lines.len() {
        if indent(lines[i]) != 0 || !item(lines[i]) {
            continue;
        }
        let end = end_of(i);
        let emptied = (i + 1..end).any(|j| drop[j])
            && (i + 1..end).all(|j| drop[j] || blank(lines[j]) || lines[j].trim().starts_with('#'));
        if emptied {
            drop[i..end].iter_mut().for_each(|x| *x = true);
            if i > 0 && lines[i - 1].trim_end() == MARK {
                drop[i - 1] = true;
            }
        }
    }
    lines
        .iter()
        .zip(drop)
        .filter(|(_, d)| !d)
        .map(|(l, _)| *l)
        .collect()
}

/// A version as dsh prints it, `0.2.1-rc.5`, in an order where a release
/// comes after its release candidates; `None` for anything else.
fn version_of(text: &str) -> Option<(u32, u32, u32, Pre)> {
    let v = text.trim();
    if v.contains(char::is_whitespace) {
        return None;
    }
    let (core, pre) = match v.split_once('-') {
        Some((c, p)) => (c, Some(p)),
        None => (v, None),
    };
    let mut n = core.split('.').map(|p| p.parse::<u32>().ok());
    let (a, b, c) = (n.next()??, n.next()??, n.next()??);
    if n.next().is_some() {
        return None;
    }
    let pre = match pre {
        None => Pre::Release,
        Some(p) => {
            let (tag, num) = p.split_once('.').unwrap_or((p, "0"));
            Pre::Tag(tag.to_string(), num.parse().ok()?)
        }
    };
    Some((a, b, c, pre))
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Pre {
    Tag(String, u32),
    Release,
}

#[cfg(test)]
mod tests {
    use super::*;

    const USER: &str = "- insert:\n    - id: mine\n      name: '@acme/dsh-x'\n";

    #[test]
    fn reads_versions() {
        let min = version_of(MIN_VERSION).unwrap();
        assert!(version_of("0.2.1-rc.5\n").unwrap() >= min);
        assert!(version_of("0.2.1-rc.4").unwrap() < min);
        assert!(version_of("0.2.1-rc.10").unwrap() > min);
        assert!(version_of("0.2.1").unwrap() > min);
        assert!(version_of("0.2.2").unwrap() > min);
        assert!(version_of("0.2.0").unwrap() < min);
        assert_eq!(
            version_of("Distributed Shell / Dancer's shell. version 0.25.10"),
            None
        );
        assert_eq!(version_of("unknown"), None);
    }

    #[test]
    fn adds_and_removes_its_entry() {
        let plugin = Path::new("/h/.dsh/imbo/plugin.mjs");
        let once = with_ours(USER, plugin);
        assert!(once.starts_with(USER));
        assert!(once.ends_with("- id: imbo\n      name: '/h/.dsh/imbo/plugin.mjs'\n"));
        assert_eq!(with_ours(&once, plugin), once);
        assert_eq!(without_ours(&once), USER);
        assert_eq!(without_ours(&with_ours("", plugin)), "");
        assert_eq!(without_ours(USER), USER);
    }

    #[test]
    fn keeps_what_the_user_put_beside_its_entry() {
        let text = "- insert:\n    - id: mine\n      name: x\n    - name: '/p'\n      id: imbo\n      config:\n        id: imbo\n- remove: [y]\n";
        assert_eq!(
            without_ours(text),
            "- insert:\n    - id: mine\n      name: x\n- remove: [y]\n"
        );
    }

    #[test]
    fn tells_a_list_from_anything_else() {
        assert!(is_list(""));
        assert!(is_list(USER));
        assert!(is_list("# mine\n---\n- insert: []\n"));
        assert!(!is_list("plugins: {}\n"));
        assert!(!is_list("[]\n"));
    }

    #[test]
    fn plugin_has_every_value_filled_in() {
        let text = plugin_code(&Home::resolve());
        assert!(
            !text.contains("__IMBO__") && !text.contains("__ENV__") && !text.contains("__NOTE__")
        );
        assert!(text.contains("\"IMBO_HOST\":\"deepseek-harness\""));
    }
}
