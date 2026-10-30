//! FLOW-WAKE: a message from me in the App or on the web opens my closed
//! Claude Code session on its computer. Tries are kept in memory only
//! (ADR-049).

use std::collections::HashMap;
use std::sync::Mutex;

use chrono::{DateTime, Utc};
use imbo_proto::api::WakeView;
use imbo_proto::frames::ServerFrame;
use uuid::Uuid;

use crate::state::St;

/// Computers on older versions ignore `wake`.
const MIN_CLIENT: (u32, u32, u32) = (0, 2, 14);
/// A try with no answer after this long counts as failed.
const ANSWER_SECS: i64 = 60;
const NO_ANSWER: &str = "The computer didn't answer.";

struct Try {
    account_id: i32,
    at: DateTime<Utc>,
    error: Option<String>,
}

#[derive(Default)]
pub struct Wakes(Mutex<HashMap<Uuid, Try>>);

impl Wakes {
    /// API-014's `wake` for an offline session.
    pub fn view(&self, agent: Uuid) -> Option<WakeView> {
        let map = self.0.lock().unwrap();
        let t = map.get(&agent)?;
        let failed = |detail: &str| WakeView {
            state: "failed".into(),
            detail: Some(detail.into()),
            at: t.at,
        };
        Some(match &t.error {
            Some(e) => failed(e),
            None if unanswered(t) => failed(NO_ANSWER),
            None => WakeView {
                state: "waking".into(),
                detail: None,
                at: t.at,
            },
        })
    }

    /// The computer's `woken`; a success means the session is online, and
    /// presence clears the try.
    pub fn answer(&self, account_id: i32, agent: Uuid, error: Option<String>) {
        let mut map = self.0.lock().unwrap();
        if let Some(t) = map.get_mut(&agent).filter(|t| t.account_id == account_id) {
            match error {
                Some(e) => t.error = Some(e),
                None => {
                    map.remove(&agent);
                }
            }
        }
    }

    /// Sessions that came online are done waking.
    pub fn online(&self, agents: &[Uuid]) {
        let mut map = self.0.lock().unwrap();
        for a in agents {
            map.remove(a);
        }
    }
}

fn unanswered(t: &Try) -> bool {
    t.error.is_none() && (Utc::now() - t.at).num_seconds() > ANSWER_SECS
}

/// Whether a computer on `version` can open sessions (API-014 `wakeable`).
pub fn supports(version: Option<&str>) -> bool {
    let Some(v) = version else { return false };
    let mut parts = v.split(['.', '-']).map(|p| p.parse::<u32>().unwrap_or(0));
    let v = (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    );
    v >= MIN_CLIENT
}

/// After I send one of my sessions a message from the App or the web: if it
/// is a closed Claude Code session on a connected computer, and no try is
/// under way, ask that computer to open it.
pub async fn request(st: &St, account_id: i32, agent: Uuid) -> Result<(), sqlx::Error> {
    let row: Option<(Uuid, String, String, Option<String>)> = sqlx::query_as(
        "SELECT a.device_id, a.tool, a.session_id, d.client_version FROM agents a
         JOIN devices d ON d.id = a.device_id
         WHERE a.id = $1 AND a.account_id = $2 AND NOT a.online AND d.revoked_at IS NULL",
    )
    .bind(agent)
    .bind(account_id)
    .fetch_optional(&st.db)
    .await?;
    let Some((device, tool, session_id, version)) = row else {
        return Ok(());
    };
    if tool != imbo_proto::TOOL_CLAUDE || !st.hub.connected(device) || !supports(version.as_deref())
    {
        return Ok(());
    }
    {
        let mut map = st.wakes.0.lock().unwrap();
        if map
            .get(&agent)
            .is_some_and(|t| t.error.is_none() && !unanswered(t))
        {
            return Ok(());
        }
        map.insert(
            agent,
            Try {
                account_id,
                at: Utc::now(),
                error: None,
            },
        );
    }
    let frame = ServerFrame::Wake {
        agent_id: agent,
        tool,
        session_id,
    };
    if !st.hub.send(device, frame) {
        st.wakes.0.lock().unwrap().remove(&agent);
    }
    tracing::info!(%agent, %device, "wake");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_computers_on_0_2_14_or_later_open_sessions() {
        assert!(supports(Some("0.2.14")));
        assert!(supports(Some("0.3.0")));
        assert!(supports(Some("1.0.0-rc.1")));
        assert!(!supports(Some("0.2.13")));
        assert!(!supports(Some("")));
        assert!(!supports(None));
    }

    #[test]
    fn a_try_shows_waking_then_the_answer() {
        let w = Wakes::default();
        let a = Uuid::now_v7();
        let fresh = |at| Try {
            account_id: 1,
            at,
            error: None,
        };
        w.0.lock().unwrap().insert(a, fresh(Utc::now()));
        assert_eq!(w.view(a).unwrap().state, "waking");
        w.answer(2, a, Some("not mine".into()));
        assert_eq!(w.view(a).unwrap().state, "waking");
        w.answer(1, a, Some("Its folder /x is gone.".into()));
        let v = w.view(a).unwrap();
        assert_eq!(
            (v.state.as_str(), v.detail.as_deref()),
            ("failed", Some("Its folder /x is gone."))
        );
        w.online(&[a]);
        assert!(w.view(a).is_none());

        let old = Utc::now() - chrono::Duration::seconds(ANSWER_SECS + 1);
        w.0.lock().unwrap().insert(a, fresh(old));
        assert_eq!(w.view(a).unwrap().detail.as_deref(), Some(NO_ANSWER));
    }
}
