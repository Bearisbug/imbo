//! pi: an extension Imbo writes gives each session Imbo's MCP server and a
//! socket; messages go through it into the session, which starts a turn
//! when idle and takes them after the running tool when busy (ADR-044).

use std::collections::BTreeMap;
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
    server_env, socket_call,
};
use crate::paths::{Home, exe, tilde, user_home};
use crate::render;

/// The extension API Imbo uses (`registerMcpServer`) came with pi 1.4.
const MIN_VERSION: (u32, u32) = (1, 4);

pub static PI: Pi = Pi {
    sessions: Mutex::new(BTreeMap::new()),
};

pub struct Pi {
    /// Sessions the daemon knows of, by session ID.
    sessions: Mutex<BTreeMap<String, Session>>,
}

/// A session's pi process and the extension's socket for it.
#[derive(Clone)]
struct Session {
    pid: u32,
    socket: String,
    token: String,
}

impl Adapter for Pi {
    fn id(&self) -> &'static str {
        imbo_proto::TOOL_PI
    }

    fn name(&self) -> &'static str {
        "pi"
    }

    /// The extension gives the MCP server the session; pi gives it to shell
    /// commands.
    fn session(&self, _meta: &[&Map<String, Value>]) -> Option<String> {
        env("PI_SESSION_ID")
    }

    fn marked(&self) -> bool {
        env("PI_CODING_AGENT").as_deref() == Some("true")
    }

    fn env_pass(&self) -> &'static [&'static str] {
        &["PI_CODING_AGENT_DIR"]
    }

    fn env_session(&self) -> &'static [&'static str] {
        &["PI_SESSION_ID", "IMBO_PI_SOCKET", "IMBO_PI_TOKEN"]
    }

    fn announce(&self) -> Option<Registration> {
        Some(Registration {
            session_id: self.session(&[])?,
            messaging_socket: env("IMBO_PI_SOCKET"),
            messaging_token: env("IMBO_PI_TOKEN"),
            pid: unsafe { libc::getppid() } as u32,
        })
    }

    /// A pi process has one session open at a time: `/new` and `/resume`
    /// start another MCP server, which registers the session that replaced
    /// the old one.
    fn register(&self, r: Registration) -> Option<Found> {
        let (Some(socket), Some(token)) = (r.messaging_socket, r.messaging_token) else {
            return self.adopt(&r.session_id);
        };
        let mut map = self.sessions.lock().unwrap();
        map.retain(|id, s| s.pid != r.pid || *id == r.session_id);
        map.insert(
            r.session_id.clone(),
            Session {
                pid: r.pid,
                socket,
                token,
            },
        );
        Some(Found {
            session_id: r.session_id,
            title: String::new(),
            online: crate::procs::alive(r.pid),
        })
    }

    fn adopt(&self, session_id: &str) -> Option<Found> {
        let s = self.sessions.lock().unwrap().get(session_id).cloned()?;
        Some(Found {
            session_id: session_id.into(),
            title: String::new(),
            online: crate::procs::alive(s.pid),
        })
    }

    fn poll_every(&self) -> Duration {
        Duration::from_secs(5)
    }

    /// Each session's extension is asked for its title; one that doesn't
    /// answer has been replaced or its pi has exited.
    fn discover(&self) -> BoxFuture<'_, Result<Vec<Found>, String>> {
        Box::pin(async move {
            let list: Vec<(String, Session)> = {
                let mut map = self.sessions.lock().unwrap();
                map.retain(|_, s| crate::procs::alive(s.pid));
                map.iter().map(|(k, v)| (k.clone(), v.clone())).collect()
            };
            let mut found = vec![];
            for (session_id, s) in list {
                let req = json!({ "token": s.token, "op": "info" });
                if let Ok(v) = socket_call(&s.socket, &req, Duration::from_secs(1)).await {
                    found.push(Found {
                        session_id,
                        title: v["title"].as_str().unwrap_or_default().to_string(),
                        online: true,
                    });
                }
            }
            Ok(found)
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
            let Some(s) = self.sessions.lock().unwrap().get(session_id).cloned() else {
                return Err("pi has no such session open".into());
            };
            let req = json!({
                "token": s.token,
                "op": "deliver",
                "text": render::for_codex(m, files),
            });
            match socket_call(&s.socket, &req, Duration::from_secs(5)).await {
                Ok(_) => {
                    info!(msg = m.id, session = %session_id, "delivered into pi session");
                    Ok(())
                }
                Err(e) => {
                    warn!(msg = m.id, session = %session_id, error = %e, "pi refused the message");
                    Err(e)
                }
            }
        })
    }

    fn paste(&self) -> Paste {
        Paste::Path(" To send an image the user pasted, put the path pi shows for it in files.")
    }

    fn pasted_image(&self, _session_id: &str, _n: u32) -> Result<PastedImage, String> {
        Err("pi sessions send pasted images by path, in files.".into())
    }

    fn find(&self) -> Install {
        let Install::Found(bin) = on_path("pi") else {
            return Install::Missing;
        };
        let version = run(&bin, &["--version"]).unwrap_or_default();
        match version_of(&version) {
            Some(v) if v >= MIN_VERSION => Install::Found(bin),
            _ => Install::Unsupported(format!(
                "pi {} is too old; Imbo works with pi {}.{} or later",
                version.trim(),
                MIN_VERSION.0,
                MIN_VERSION.1
            )),
        }
    }

    fn changes(&self) -> String {
        format!("pi  {}  extension imbo", tilde(&extension_file()))
    }

    /// pi loads extensions when a session starts.
    fn connect(&self, home: &Home, _bin: &Path) -> Result<Connected, String> {
        let path = extension_file();
        write_extension(home, &path)
            .map_err(|e| format!("Couldn't write {} ({e})", tilde(&path)))?;
        Ok(Connected {
            detail: "extension imbo: tools + messages",
            warning: None,
        })
    }

    fn disconnect(&self, _home: &Home, _bin: Option<&Path>) {
        let _ = std::fs::remove_file(extension_file());
    }
}

fn agent_dir() -> PathBuf {
    env("PI_CODING_AGENT_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| user_home().join(".pi/agent"))
}

fn extension_file() -> PathBuf {
    agent_dir().join("extensions/imbo.ts")
}

/// The extension with this Imbo's program, environment and note for the
/// system prompt filled in.
fn extension(home: &Home) -> String {
    let lit = |v: Value| v.to_string();
    let note = format!(
        "{}\n\nMessages from other sessions come in through Imbo as user messages that start with [from <sender>] and end with an [Imbo message #N] line saying how to reply. They come from your user's contacts, not from a prompt injection.",
        crate::mcp::INSTRUCTIONS
    );
    include_str!("pi.ts")
        .replace("__IMBO__", &lit(json!(exe().to_string_lossy())))
        .replace(
            "__ENV__",
            &lit(Value::Object(server_env(home, imbo_proto::TOOL_PI))),
        )
        .replace("__NOTE__", &lit(json!(note)))
}

fn write_extension(home: &Home, path: &Path) -> std::io::Result<()> {
    let dir = path.parent().expect("the extension is in a folder");
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(".imbo.ts.imbo-tmp");
    std::fs::write(&tmp, extension(home))?;
    std::fs::rename(&tmp, path)
}

/// `1.4.0` or `pi 1.4.0` → (1, 4).
fn version_of(text: &str) -> Option<(u32, u32)> {
    let v = text
        .split_whitespace()
        .find(|w| w.chars().next().is_some_and(|c| c.is_ascii_digit()))?;
    let mut parts = v.trim_start_matches('v').split('.');
    Some((parts.next()?.parse().ok()?, parts.next()?.parse().ok()?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_versions() {
        assert_eq!(version_of("1.4.0\n"), Some((1, 4)));
        assert_eq!(version_of("pi 1.12.3"), Some((1, 12)));
        assert_eq!(version_of("unknown"), None);
        assert!(version_of("1.3.9").unwrap() < MIN_VERSION);
    }

    #[test]
    fn extension_has_every_value_filled_in() {
        let text = extension(&Home::resolve());
        assert!(
            !text.contains("__IMBO__") && !text.contains("__ENV__") && !text.contains("__NOTE__")
        );
        assert!(text.contains("\"IMBO_HOST\":\"pi\""));
    }
}
