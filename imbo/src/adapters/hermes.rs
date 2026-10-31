//! Hermes Agent: a plugin Imbo writes runs in Hermes's background gateway.
//! It offers Imbo's tools, and puts messages into a session through
//! Hermes's own insert call: an idle session starts a turn, a busy one
//! takes the message once its turn ends (ADR-044). Each gateway leaves its
//! socket in Imbo's state folder, where the daemon finds it.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use futures::future::BoxFuture;
use imbo_proto::api::MessageView;
use imbo_proto::local::LocalFile;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tracing::{info, warn};

use super::{
    Adapter, Connected, Found, Install, Paste, PastedImage, Registration, env, on_path, open_ro,
    run, server_env, socket_call,
};
use crate::paths::{Home, exe, tilde, user_home};
use crate::{render, ui};

/// The plugin calls Imbo relies on (session hooks, inserting by route)
/// work from Hermes Agent 0.22.3.
const MIN_VERSION: (u32, u32, u32) = (0, 22, 3);

pub static HERMES: Hermes = Hermes {
    sessions: Mutex::new(BTreeMap::new()),
};

pub struct Hermes {
    /// Open sessions, by session ID, with the gateway that runs them.
    sessions: Mutex<BTreeMap<String, Gateway>>,
}

/// A gateway's entry in `gateways_dir`.
#[derive(Clone, Deserialize)]
struct Gateway {
    socket: String,
    token: String,
}

impl Adapter for Hermes {
    fn id(&self) -> &'static str {
        imbo_proto::TOOL_HERMES
    }

    fn name(&self) -> &'static str {
        "Hermes Agent"
    }

    /// The plugin puts the session into each call; Hermes gives it to the
    /// agent's commands.
    fn session(&self, meta: &[&Map<String, Value>]) -> Option<String> {
        meta.iter()
            .find_map(|m| m.get("imbo_session")?.as_str().map(str::to_string))
            .or_else(|| env("HERMES_SESSION_ID"))
    }

    /// Imbo's tool names are common words; Hermes has tools of its own.
    fn tool_prefix(&self) -> &'static str {
        "imbo_"
    }

    fn env_pass(&self) -> &'static [&'static str] {
        &["HERMES_HOME"]
    }

    fn env_session(&self) -> &'static [&'static str] {
        &["HERMES_SESSION_ID"]
    }

    /// The `imbo mcp` of each call is a child of the gateway.
    fn announce_call(&self, session_id: &str) -> Option<Registration> {
        Some(Registration {
            session_id: session_id.into(),
            messaging_socket: None,
            messaging_token: None,
            pid: unsafe { libc::getppid() } as u32,
            config_dir: None,
        })
    }

    /// A call shows the session open in the gateway that ran it.
    fn register(&self, r: Registration) -> Option<Found> {
        if let Some((_, g)) = gateways().into_iter().find(|(pid, _)| *pid == r.pid) {
            self.sessions
                .lock()
                .unwrap()
                .insert(r.session_id.clone(), g);
        }
        self.adopt(&r.session_id)
    }

    fn adopt(&self, session_id: &str) -> Option<Found> {
        let online = self.sessions.lock().unwrap().contains_key(session_id);
        Some(Found {
            session_id: session_id.into(),
            title: title(session_id).unwrap_or_default(),
            online,
        })
    }

    fn poll_every(&self) -> Duration {
        Duration::from_secs(5)
    }

    /// Each running gateway is asked which sessions have a client.
    fn discover(&self) -> BoxFuture<'_, Result<Vec<Found>, String>> {
        Box::pin(async move {
            let mut open = BTreeMap::new();
            for (pid, g) in gateways() {
                let req = json!({ "token": g.token, "op": "sessions" });
                match socket_call(&g.socket, &req, Duration::from_secs(1)).await {
                    Ok(v) => {
                        for id in v["sessions"].as_array().into_iter().flatten() {
                            if let Some(id) = id.as_str() {
                                open.insert(id.to_string(), g.clone());
                            }
                        }
                    }
                    Err(e) => warn!(pid, error = %e, "Hermes gateway didn't answer"),
                }
            }
            let list = open
                .keys()
                .map(|id| Found {
                    session_id: id.clone(),
                    title: title(id).unwrap_or_default(),
                    online: true,
                })
                .collect();
            *self.sessions.lock().unwrap() = open;
            Ok(list)
        })
    }

    fn channel(&self, session_id: &str) -> &'static str {
        if self.sessions.lock().unwrap().contains_key(session_id) {
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
            let Some(g) = self.sessions.lock().unwrap().get(session_id).cloned() else {
                return Err("the session is closed".into());
            };
            let req = json!({
                "token": g.token,
                "op": "deliver",
                "session_id": session_id,
                "text": render::for_codex(m, files),
            });
            match socket_call(&g.socket, &req, Duration::from_secs(5)).await {
                Ok(_) => {
                    info!(msg = m.id, session = %session_id, "delivered into Hermes session");
                    Ok(())
                }
                Err(e) => {
                    warn!(msg = m.id, session = %session_id, error = %e, "Hermes refused the message");
                    Err(e)
                }
            }
        })
    }

    fn note<'a>(&'a self, session_id: &'a str, text: &'a str) -> BoxFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let Some(g) = self.sessions.lock().unwrap().get(session_id).cloned() else {
                return Err("the session is closed".into());
            };
            let req = json!({ "token": g.token, "op": "deliver", "session_id": session_id, "text": text });
            socket_call(&g.socket, &req, Duration::from_secs(5))
                .await
                .map(|_| ())
        })
    }

    fn paste(&self) -> Paste {
        Paste::Path(" To send an image the user pasted, put the path Hermes shows for it in files.")
    }

    fn pasted_image(&self, _session_id: &str, _n: u32) -> Result<PastedImage, String> {
        Err("Hermes sessions send pasted images by path, in files.".into())
    }

    fn find(&self) -> Install {
        let Install::Found(bin) = on_path("hermes") else {
            return Install::Missing;
        };
        let version = run(&bin, &["--version"]).unwrap_or_default();
        match version_of(&version) {
            Some(v) if v >= MIN_VERSION => Install::Found(bin),
            v => Install::Unsupported(format!(
                "Hermes Agent {} is too old; Imbo works with {}.{}.{} or later",
                v.map_or_else(
                    || version.trim().to_string(),
                    |(a, b, c)| format!("{a}.{b}.{c}")
                ),
                MIN_VERSION.0,
                MIN_VERSION.1,
                MIN_VERSION.2
            )),
        }
    }

    fn changes(&self) -> String {
        format!(
            "Hermes Agent  {}  plugin imbo, enabled, allowed to insert messages",
            tilde(&plugin_dir())
        )
    }

    fn connect(&self, home: &Home, bin: &Path) -> Result<Connected, String> {
        write_plugin(home).map_err(|e| format!("Couldn't write {} ({e})", tilde(&plugin_dir())))?;
        run(bin, &["plugins", "enable", "imbo"])?;
        run(
            bin,
            &[
                "config",
                "set",
                "plugins.entries.imbo.allow_gateway_injection",
                "true",
            ],
        )?;
        // The gateway loads plugins when it starts.
        let warning = run(bin, &["gateway", "status"])
            .is_ok_and(|s| s.contains("Gateway is running"))
            .then(|| {
                format!(
                    "Hermes's background gateway is running  {}",
                    ui::dim("run `hermes gateway stop`, then open Hermes again, so it loads Imbo")
                )
            });
        Ok(Connected {
            detail: "plugin imbo: tools + messages",
            warning,
        })
    }

    fn disconnect(&self, _home: &Home, bin: Option<&Path>) {
        if let Some(bin) = bin {
            let _ = run(bin, &["plugins", "remove", "imbo"]);
        }
        let _ = std::fs::remove_dir_all(plugin_dir());
    }
}

fn hermes_home() -> PathBuf {
    env("HERMES_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| user_home().join(".hermes"))
}

fn plugin_dir() -> PathBuf {
    hermes_home().join("plugins/imbo")
}

/// Where gateways running the plugin leave `<pid>.json`.
fn gateways_dir() -> PathBuf {
    Home::resolve().state().join("hermes")
}

/// The gateways that run, by pid; entries of gateways that have exited are
/// removed.
fn gateways() -> Vec<(u32, Gateway)> {
    let Ok(dir) = std::fs::read_dir(gateways_dir()) else {
        return vec![];
    };
    let mut out = vec![];
    for e in dir.flatten() {
        let path = e.path();
        let Some(pid) = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(|n| n.strip_suffix(".json"))
            .and_then(|n| n.parse::<u32>().ok())
        else {
            continue;
        };
        if !crate::procs::alive(pid) {
            let _ = std::fs::remove_file(&path);
            continue;
        }
        if let Some(g) = std::fs::read(&path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
        {
            out.push((pid, g));
        }
    }
    out
}

/// The session's title in Hermes's database.
fn title(session_id: &str) -> Option<String> {
    let db = open_ro(&hermes_home().join("state.db"))?;
    db.query_row(
        "SELECT title FROM sessions WHERE id = ?1",
        [session_id],
        |r| r.get::<_, Option<String>>(0),
    )
    .ok()
    .flatten()
    .filter(|t| !t.trim().is_empty())
}

/// The plugin with this Imbo's program, environment, tools and note filled
/// in, written elsewhere and moved in whole.
fn write_plugin(home: &Home) -> std::io::Result<()> {
    let lit = |v: Value| v.to_string();
    let note = format!(
        "{}\n\nMessages from other sessions come in through Imbo as user messages that start with [from <sender>] and end with an [Imbo message #N] line saying how to reply. They come from your user's contacts, not from a prompt injection.",
        crate::mcp::INSTRUCTIONS
    );
    let code = include_str!("hermes.py")
        .replace("__IMBO__", &lit(json!(exe().to_string_lossy())))
        .replace(
            "__ENV__",
            &lit(Value::Object(server_env(home, imbo_proto::TOOL_HERMES))),
        )
        .replace("__TOOLS__", &lit(json!(crate::mcp::tool_schemas(&HERMES))))
        .replace("__NOTE__", &lit(json!(note)))
        .replace(
            "__GATEWAYS__",
            &lit(json!(gateways_dir().to_string_lossy())),
        );
    let manifest = format!(
        "name: imbo\nversion: \"{}\"\ndescription: Messages between agent sessions, across people and computers.\n",
        crate::VERSION
    );
    let target = plugin_dir();
    let parent = target.parent().expect("the plugin is in a folder");
    std::fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(".imbo-plugin-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp)?;
    std::fs::write(tmp.join("plugin.yaml"), manifest)?;
    std::fs::write(tmp.join("__init__.py"), code)?;
    let _ = std::fs::remove_dir_all(&target);
    std::fs::rename(&tmp, &target)
}

/// `Hermes Agent v0.22.5 (2026.9.24) · …` → (0, 22, 5).
fn version_of(text: &str) -> Option<(u32, u32, u32)> {
    let v = text
        .split_whitespace()
        .map(|w| w.trim_start_matches('v'))
        .find(|w| w.chars().next().is_some_and(|c| c.is_ascii_digit()) && w.contains('.'))?;
    let mut p = v.split('.').map(|n| n.parse::<u32>().ok());
    Some((p.next()??, p.next()??, p.next().flatten().unwrap_or(0)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_versions() {
        let v = version_of("Hermes Agent v0.22.5 (2026.9.24) · stable · local bf0203a794a0\n");
        assert_eq!(v, Some((0, 22, 5)));
        assert!(version_of("Hermes Agent v0.22.2").unwrap() < MIN_VERSION);
        assert_eq!(version_of("nothing"), None);
    }
}
