//! Which agent sessions are open on this computer.

use std::collections::HashMap;

use imbo_proto::frames::PresenceItem;
use imbo_proto::local::LocalSession;
use imbo_proto::text::{clean_title, short_id};

use crate::adapters::{self, Found, Registration};

pub type Key = (String, String);

#[derive(Debug, Clone)]
pub struct Entry {
    pub tool: String,
    pub session_id: String,
    pub title: String,
    pub online: bool,
    /// Pinned by API-LOCAL-007 with no live tool behind it.
    pub fake: bool,
}

impl Entry {
    /// How messages reach this session right now.
    pub fn channel(&self) -> &'static str {
        match adapters::get(&self.tool) {
            Some(a) if !self.fake => a.channel(&self.session_id),
            _ => "none",
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

    /// What an adapter found, kept as a real session; an empty title keeps
    /// the one already known.
    fn put(&mut self, tool: &str, f: Found) {
        let key = (tool.to_string(), f.session_id.clone());
        let e = self.map.entry(key).or_insert_with(|| Entry {
            tool: tool.into(),
            session_id: f.session_id,
            title: String::new(),
            online: f.online,
            fake: false,
        });
        e.fake = false;
        e.online = f.online;
        if !f.title.is_empty() {
            e.title = f.title;
        }
    }

    /// API-LOCAL-001. `false` when the tool has no adapter.
    pub fn register(&mut self, tool: &str, r: Registration) -> bool {
        let Some(a) = adapters::get(tool) else {
            return false;
        };
        if let Some(f) = a.register(r) {
            self.put(tool, f);
        }
        true
    }

    /// A session that called us but isn't known (CLI inside a session, or a
    /// Codex session outside the shared app-server). Returns whether it is
    /// online now.
    pub fn adopt(&mut self, tool: &str, session_id: &str) -> bool {
        let key = (tool.to_string(), session_id.to_string());
        if let Some(e) = self.map.get(&key).filter(|e| e.fake) {
            return e.online;
        }
        let Some(f) = adapters::get(tool).and_then(|a| a.adopt(session_id)) else {
            return self.map.get(&key).is_some_and(|e| e.online);
        };
        let online = f.online;
        self.put(tool, f);
        online
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
                fake: true,
            },
        );
    }

    /// A tool's discovery result: its sessions not in `found` are closed.
    pub fn apply(&mut self, tool: &str, found: Vec<Found>) {
        for e in self.map.values_mut() {
            if e.tool == tool && !e.fake {
                e.online = false;
            }
        }
        for f in found {
            let key = (tool.to_string(), f.session_id.clone());
            if !self.map.get(&key).is_some_and(|e| e.fake) {
                self.put(tool, f);
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

#[cfg(test)]
mod tests {
    use super::*;

    fn found(id: &str, title: &str) -> Found {
        Found {
            session_id: id.into(),
            title: title.into(),
            online: true,
        }
    }

    #[test]
    fn discovery_closes_missing_sessions_and_leaves_pinned_ones() {
        let key = |id: &str| ("codex".to_string(), id.to_string());
        let mut s = Sessions::default();
        s.set_fake("codex", "pin", "pinned", false);
        s.apply("codex", vec![found("a", "Alpha"), found("b", "Beta")]);
        assert_eq!(s.changes().len(), 3);

        s.apply("codex", vec![found("a", "")]);
        let a = s.get(&key("a")).unwrap();
        assert_eq!((a.title.as_str(), a.online), ("Alpha", true));
        assert!(s.get(&key("pin")).unwrap().online);
        assert_eq!(s.get(&key("pin")).unwrap().channel(), "none");
        let gone = s.changes();
        assert_eq!(gone.len(), 1);
        assert_eq!((gone[0].session_id.as_str(), gone[0].online), ("b", false));
        assert!(s.get(&key("b")).is_none());
    }
}
