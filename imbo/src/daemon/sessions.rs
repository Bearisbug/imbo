//! Which agent sessions are open on this computer.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use imbo_proto::frames::PresenceItem;
use imbo_proto::local::LocalSession;
use imbo_proto::text::{clean_title, short_id};
use imbo_proto::{TOOL_CLAUDE, TOOL_CODEX};

use crate::adapters::{claude, codex};

/// Inserts that may fail in a row before a Claude Code session falls back to
/// the prompt hook (FLOW-DELIVER).
pub const MAX_INJECT_FAILURES: u32 = 3;
/// A Codex session seen only through its own tool calls stays online this long.
const ADOPTED_TTL: Duration = Duration::from_secs(30 * 60);

pub type Key = (String, String);

#[derive(Debug, Clone)]
pub enum Kind {
    Claude {
        pid: u32,
        socket: Option<String>,
        token: Option<String>,
        failures: u32,
    },
    Codex {
        loaded: bool,
        adopted_at: Option<Instant>,
    },
    Fake,
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub tool: String,
    pub session_id: String,
    pub title: String,
    pub online: bool,
    pub kind: Kind,
}

impl Entry {
    /// How messages reach this session right now.
    pub fn channel(&self) -> &'static str {
        match &self.kind {
            Kind::Claude {
                socket: Some(_),
                token: Some(_),
                failures,
                ..
            } if *failures < MAX_INJECT_FAILURES => "inject",
            Kind::Claude { .. } => "prompt_hook",
            Kind::Codex { loaded: true, .. } => "inject",
            Kind::Codex { .. } => "queue",
            Kind::Fake => "none",
        }
    }

    fn presence(&self) -> PresenceItem {
        PresenceItem {
            tool: self.tool.clone(),
            session_id: self.session_id.clone(),
            title: clean_title(&self.title, &self.session_id),
            online: self.online,
        }
    }
}

#[derive(Default)]
pub struct Sessions {
    pub map: HashMap<Key, Entry>,
    /// What the server was last told: title and online flag.
    reported: HashMap<Key, (String, bool)>,
}

impl Sessions {
    pub fn get(&self, key: &Key) -> Option<&Entry> {
        self.map.get(key)
    }

    pub fn get_mut(&mut self, key: &Key) -> Option<&mut Entry> {
        self.map.get_mut(key)
    }

    /// API-LOCAL-001 from a Claude Code session's MCP process.
    pub fn register_claude(
        &mut self,
        session_id: &str,
        pid: u32,
        socket: Option<String>,
        token: Option<String>,
    ) {
        let title = claude::title(pid, session_id).unwrap_or_default();
        let key = (TOOL_CLAUDE.to_string(), session_id.to_string());
        let entry = self.map.entry(key).or_insert_with(|| Entry {
            tool: TOOL_CLAUDE.into(),
            session_id: session_id.into(),
            title: String::new(),
            online: true,
            kind: Kind::Fake,
        });
        let failures = match &entry.kind {
            Kind::Claude {
                pid: p,
                socket: s,
                token: t,
                failures,
            } if *p == pid && *s == socket && *t == token => *failures,
            _ => 0,
        };
        entry.kind = Kind::Claude {
            pid,
            socket,
            token,
            failures,
        };
        entry.online = claude::alive(pid);
        if !title.is_empty() {
            entry.title = title;
        }
    }

    /// A session that called us but isn't known (CLI inside a session, or a
    /// Codex session outside the shared app-server).
    pub fn adopt(&mut self, tool: &str, session_id: &str) -> bool {
        let key = (tool.to_string(), session_id.to_string());
        if let Some(e) = self.map.get_mut(&key) {
            if let Kind::Codex { adopted_at, .. } = &mut e.kind {
                *adopted_at = Some(Instant::now());
                e.online = true;
            }
            return e.online;
        }
        let entry = match tool {
            TOOL_CLAUDE => {
                let Some(pid) = claude::pid_for(session_id) else {
                    return false;
                };
                Entry {
                    tool: tool.into(),
                    session_id: session_id.into(),
                    title: claude::title(pid, session_id).unwrap_or_default(),
                    online: true,
                    kind: Kind::Claude {
                        pid,
                        socket: None,
                        token: None,
                        failures: 0,
                    },
                }
            }
            TOOL_CODEX => Entry {
                tool: tool.into(),
                session_id: session_id.into(),
                title: String::new(),
                online: true,
                kind: Kind::Codex {
                    loaded: false,
                    adopted_at: Some(Instant::now()),
                },
            },
            _ => return false,
        };
        self.map.insert(key, entry);
        true
    }

    pub fn set_fake(&mut self, tool: &str, session_id: &str, title: &str, remove: bool) {
        let key = (tool.to_string(), session_id.to_string());
        if remove {
            if let Some(e) = self.map.get_mut(&key) {
                e.online = false;
            }
            return;
        }
        self.map.insert(
            key,
            Entry {
                tool: tool.into(),
                session_id: session_id.into(),
                title: title.into(),
                online: true,
                kind: Kind::Fake,
            },
        );
    }

    /// Re-check Claude Code processes and titles.
    pub fn refresh_claude(&mut self) {
        for e in self.map.values_mut() {
            if let Kind::Claude { pid, .. } = e.kind {
                e.online = claude::alive(pid);
                if e.online
                    && let Some(t) = claude::title(pid, &e.session_id) {
                        e.title = t;
                    }
            }
        }
    }

    /// Apply the shared app-server's loaded threads (`None`: app-server not running).
    pub fn apply_codex(&mut self, threads: Option<Vec<codex::Thread>>) {
        let threads = threads.unwrap_or_default();
        let now = Instant::now();
        for e in self.map.values_mut() {
            if let Kind::Codex { loaded, adopted_at } = &mut e.kind {
                *loaded = false;
                e.online = adopted_at.is_some_and(|t| now.duration_since(t) < ADOPTED_TTL);
            }
        }
        for t in threads {
            let key = (TOOL_CODEX.to_string(), t.id.clone());
            let e = self.map.entry(key).or_insert_with(|| Entry {
                tool: TOOL_CODEX.into(),
                session_id: t.id.clone(),
                title: String::new(),
                online: true,
                kind: Kind::Codex {
                    loaded: true,
                    adopted_at: None,
                },
            });
            if let Kind::Codex { loaded, .. } = &mut e.kind {
                *loaded = true;
            }
            e.online = true;
            if !t.title.is_empty() {
                e.title = t.title;
            }
        }
    }

    /// Everything online, for `hello`. Marks it all as reported.
    pub fn hello(&mut self) -> Vec<PresenceItem> {
        self.reported.clear();
        let mut items = vec![];
        for (k, e) in &self.map {
            if e.online {
                let p = e.presence();
                self.reported.insert(k.clone(), (p.title.clone(), true));
                items.push(p);
            }
        }
        items
    }

    /// Changes since the last report. Offline entries that were reported are
    /// dropped from the map once their offline state is sent.
    pub fn changes(&mut self) -> Vec<PresenceItem> {
        let mut items = vec![];
        for (k, e) in &self.map {
            let p = e.presence();
            let before = self.reported.get(k);
            let changed = match before {
                None => e.online,
                Some((title, online)) => *online != e.online || (e.online && *title != p.title),
            };
            if changed {
                items.push(p);
            }
        }
        for p in &items {
            self.reported.insert(
                (p.tool.clone(), p.session_id.clone()),
                (p.title.clone(), p.online),
            );
        }
        let stale: Vec<Key> = self
            .map
            .iter()
            .filter(|(k, e)| !e.online && self.reported.get(*k).is_none_or(|(_, on)| !on))
            .map(|(k, _)| k.clone())
            .collect();
        for k in stale {
            self.map.remove(&k);
            self.reported.remove(&k);
        }
        items
    }

    pub fn list(&self) -> Vec<LocalSession> {
        let mut v: Vec<LocalSession> = self
            .map
            .values()
            .map(|e| LocalSession {
                tool: e.tool.clone(),
                session_id: e.session_id.clone(),
                short_id: short_id(&e.session_id),
                title: clean_title(&e.title, &e.session_id),
                online: e.online,
                channel: e.channel().into(),
            })
            .collect();
        v.sort_by(|a, b| (b.online, &a.tool, &a.title).cmp(&(a.online, &b.tool, &b.title)));
        v
    }
}
