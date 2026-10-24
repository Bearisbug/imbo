//! Messages already put into a session, kept 7 days (ADR-006), so a re-push
//! after a lost ack is acked without inserting twice.

use std::collections::HashSet;
use std::io::Write;
use std::path::PathBuf;

use uuid::Uuid;

const KEEP_SECS: i64 = 7 * 24 * 3600;

pub struct Dedupe {
    path: PathBuf,
    seen: HashSet<(i64, Uuid)>,
}

impl Dedupe {
    pub fn load(path: PathBuf) -> Self {
        let now = chrono::Utc::now().timestamp();
        let mut seen = HashSet::new();
        let mut keep = String::new();
        if let Ok(s) = std::fs::read_to_string(&path) {
            for line in s.lines() {
                let mut it = line.split_whitespace();
                let (Some(m), Some(a), Some(t)) = (it.next(), it.next(), it.next()) else {
                    continue;
                };
                let (Ok(m), Ok(a), Ok(t)) =
                    (m.parse::<i64>(), Uuid::parse_str(a), t.parse::<i64>())
                else {
                    continue;
                };
                if now - t < KEEP_SECS {
                    seen.insert((m, a));
                    keep.push_str(line);
                    keep.push('\n');
                }
            }
            let _ = std::fs::write(&path, keep);
        }
        Self { path, seen }
    }

    pub fn contains(&self, message: i64, agent: Uuid) -> bool {
        self.seen.contains(&(message, agent))
    }

    pub fn add(&mut self, message: i64, agent: Uuid) {
        if self.seen.insert((message, agent))
            && let Ok(mut f) = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.path)
        {
            let _ = writeln!(f, "{message} {agent} {}", chrono::Utc::now().timestamp());
        }
    }
}
