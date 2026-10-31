//! Talking a group message over (FLOW-GROUP-CONSULT S5): a session given a
//! group message that its person's other sessions got too, and that hasn't
//! spoken in the group 3 minutes later, is reminded to decide on its own.

use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use imbo_proto::TOOL_CURSOR;
use imbo_proto::api::{GroupId, MessageView};
use tracing::info;

use super::D;
use super::listen;
use super::sessions::Key;
use crate::adapters;

const WAIT: Duration = Duration::from_secs(180);

/// Reminders waiting, by session and group: `true` once the session spoke
/// there.
static PENDING: LazyLock<Mutex<HashMap<(Key, GroupId), bool>>> = LazyLock::new(Default::default);

/// A message went into the session. Several in the same group within the
/// 3 minutes get one reminder, timed from the first.
pub fn delivered(d: &D, key: &Key, m: &MessageView) {
    let Some(g) = &m.conversation.group else {
        return;
    };
    if m.siblings.is_empty() {
        return;
    }
    let k = (key.clone(), g.id);
    {
        let mut pending = PENDING.lock().unwrap();
        if pending.contains_key(&k) {
            return;
        }
        pending.insert(k.clone(), false);
    }
    let (d, name) = (d.clone(), g.name.clone());
    tokio::spawn(async move {
        tokio::time::sleep(WAIT).await;
        if PENDING.lock().unwrap().remove(&k) != Some(false) {
            return;
        }
        remind(&d, &k.0, &name).await;
    });
}

/// The session sent a message to the group.
pub fn spoke(key: &Key, group: GroupId) {
    if let Some(s) = PENDING.lock().unwrap().get_mut(&(key.clone(), group)) {
        *s = true;
    }
}

/// Put the reminder in once; a closed session, or one it can't go into,
/// isn't reminded.
async fn remind(d: &D, key: &Key, group: &str) {
    let online = d
        .sessions
        .lock()
        .unwrap()
        .get(key)
        .is_some_and(|e| e.online && !e.fake);
    let Some(tool) = adapters::get(&key.0).filter(|_| online) else {
        return;
    };
    let text = format!(
        "Imbo: it's been 3 minutes since a message in group {group} came to this session, and it hasn't said anything there yet. Decide now, on your own, whether to reply; reply with Imbo's send tool if you do."
    );
    let r = if !tool.listens() {
        tool.note(&key.1, &text).await
    } else if key.0 != TOOL_CURSOR && listen::waiting(&key.0, &key.1) {
        listen::hand_over(key, text).await
    } else {
        return;
    };
    match r {
        Ok(()) => info!(session = %key.1, group, "reminded to decide on a group message"),
        Err(e) => info!(session = %key.1, group, error = %e, "group reminder not put in"),
    }
}
