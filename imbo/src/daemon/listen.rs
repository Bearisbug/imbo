//! Listeners (FLOW-LISTEN, API-LOCAL-010, 011): `imbo wait` and Cursor's
//! stop hook hold a connection open until messages for the session arrive,
//! print a notice, and confirm. The tool then starts a turn, and the agent
//! takes the messages with Imbo's inbox tool: that pull is the delivery.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use imbo_proto::api::{InboxResponse, MessageId, MessageView};
use imbo_proto::local::{ListenerStatus, WaitReply};
use imbo_proto::{ErrorCode, Problem};
use tokio::io::{AsyncWriteExt, BufReader, Lines};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::sync::{mpsc, oneshot};
use tracing::{info, warn};

use super::D;
use super::sessions::Key;
use crate::adapters;

/// A listener that ends sooner than this without handing anything over
/// counts as a crash (API-LOCAL-011).
const SHORT: Duration = Duration::from_secs(60);
/// How long a listener has to confirm it printed what it was given.
const CONFIRM_WITHIN: Duration = Duration::from_secs(30);
/// Notices per message; one the agent still hasn't taken after this many
/// waits for the session's next prompt (API-LOCAL-003).
const MAX_NOTICES: u8 = 2;

enum Event {
    /// Print this, confirm, end; `done` gets whether it was confirmed.
    Hand(String, oneshot::Sender<bool>),
    /// End without output: replaced by a newer listener or withdrawn.
    End,
}

struct Listener {
    id: u64,
    pid: Option<u32>,
    tx: mpsc::UnboundedSender<Event>,
}

static NEXT: AtomicU64 = AtomicU64::new(1);
static LISTENERS: LazyLock<Mutex<HashMap<Key, Listener>>> = LazyLock::new(Default::default);
/// How each session's last 3 listeners ended: `true` for a short life with
/// nothing handed over.
static ENDS: LazyLock<Mutex<HashMap<Key, VecDeque<bool>>>> = LazyLock::new(Default::default);
/// Notices given per session and message.
static NOTICES: LazyLock<Mutex<HashMap<Key, HashMap<MessageId, u8>>>> =
    LazyLock::new(Default::default);

fn key(tool: &str, session_id: &str) -> Key {
    (tool.to_string(), session_id.to_string())
}

pub fn waiting(tool: &str, session_id: &str) -> bool {
    LISTENERS
        .lock()
        .unwrap()
        .get(&key(tool, session_id))
        .is_some_and(|l| !l.tx.is_closed())
}

/// Sessions of `tool` with a listener waiting.
pub fn sessions(tool: &str) -> Vec<String> {
    LISTENERS
        .lock()
        .unwrap()
        .iter()
        .filter(|((t, _), l)| t == tool && !l.tx.is_closed())
        .map(|((_, s), _)| s.clone())
        .collect()
}

/// API-LOCAL-011.
pub fn status(tool: &str, session_id: &str) -> ListenerStatus {
    let ends = ENDS.lock().unwrap();
    let stalled = ends
        .get(&key(tool, session_id))
        .is_some_and(|e| e.len() == 3 && e.iter().all(|short| *short));
    ListenerStatus {
        listening: waiting(tool, session_id),
        stalled,
    }
}

fn record(key: &Key, short: bool) {
    let mut ends = ENDS.lock().unwrap();
    let e = ends.entry(key.clone()).or_default();
    e.push_back(short);
    while e.len() > 3 {
        e.pop_front();
    }
}

/// Whether the message may still be announced to the session.
pub fn may_notice(key: &Key, id: MessageId) -> bool {
    NOTICES
        .lock()
        .unwrap()
        .get(key)
        .and_then(|n| n.get(&id))
        .is_none_or(|c| *c < MAX_NOTICES)
}

pub fn noticed(key: &Key, ids: &[MessageId]) {
    let mut all = NOTICES.lock().unwrap();
    let n = all.entry(key.clone()).or_default();
    for id in ids {
        *n.entry(*id).or_default() += 1;
    }
}

/// The turn was aborted (API-LOCAL-010): the waiting listener ends quietly.
pub fn withdraw(tool: &str, session_id: &str) {
    if let Some(l) = LISTENERS.lock().unwrap().remove(&key(tool, session_id)) {
        let _ = l.tx.send(Event::End);
    }
}

/// Give `text` to the session's listener and wait until it confirms it
/// printed it. `Err` is the nack reason.
pub async fn hand_over(key: &Key, text: String) -> Result<(), String> {
    if adapters::fault(&key.0) {
        return Err("inject-failed: fault injected".into());
    }
    let listener = LISTENERS.lock().unwrap().remove(key);
    let Some(l) = listener else {
        return Err("no-listener".into());
    };
    let (done, confirmed) = oneshot::channel();
    if l.tx.send(Event::Hand(text, done)).is_err() {
        return Err("no-listener".into());
    }
    match tokio::time::timeout(CONFIRM_WITHIN + Duration::from_secs(5), confirmed).await {
        Ok(Ok(true)) => Ok(()),
        _ => Err("listener-lost".into()),
    }
}

type Reader = Lines<BufReader<OwnedReadHalf>>;

async fn reply(w: &mut OwnedWriteHalf, r: &WaitReply) -> bool {
    let mut line = serde_json::to_vec(r).unwrap_or_default();
    line.push(b'\n');
    w.write_all(&line).await.is_ok()
}

async fn refuse(w: &mut OwnedWriteHalf, p: Problem) {
    let res = imbo_proto::local::LocalResponse::err(p);
    let mut line = serde_json::to_vec(&res).unwrap_or_default();
    line.push(b'\n');
    let _ = w.write_all(&line).await;
}

/// Wait for the confirmation line after printed text.
async fn confirmed(lines: &mut Reader) -> bool {
    matches!(
        tokio::time::timeout(CONFIRM_WITHIN, lines.next_line()).await,
        Ok(Ok(Some(_)))
    )
}

/// API-LOCAL-010 on one connection, from registration to the end.
pub async fn serve(
    d: &D,
    tool: String,
    session_id: String,
    pid: Option<u32>,
    mut lines: Reader,
    mut w: OwnedWriteHalf,
) {
    let Some(a) = adapters::get(&tool).filter(|a| a.listens()) else {
        let p = Problem::new(
            ErrorCode::InvalidRequest,
            format!("Imbo doesn't deliver to `{tool}` sessions through a listener."),
        );
        return refuse(&mut w, p).await;
    };
    if session_id.trim().is_empty() {
        let p = Problem::new(ErrorCode::InvalidRequest, "No session to listen for.");
        return refuse(&mut w, p).await;
    }
    let key = key(&tool, &session_id);
    d.sessions.lock().unwrap().adopt(&tool, &session_id);
    d.flush_presence();
    if !a.keep_newest() && waiting(&tool, &session_id) {
        reply(
            &mut w,
            &WaitReply {
                text: None,
                already: true,
            },
        )
        .await;
        return;
    }

    // Messages that waited on the server while nobody listened, or that
    // the last turn was told of and didn't take.
    let waiting = waiting_on_server(d, &key).await;
    if waiting.iter().any(|m| may_notice(&key, m.id)) {
        let ms: Vec<&MessageView> = waiting.iter().collect();
        let text = crate::render::for_listen(&ms);
        info!(tool, session = %session_id, count = ms.len(), "listener told of waiting messages");
        if reply(
            &mut w,
            &WaitReply {
                text: Some(text),
                already: false,
            },
        )
        .await
            && confirmed(&mut lines).await
        {
            noticed(&key, &waiting.iter().map(|m| m.id).collect::<Vec<_>>());
        }
        return;
    }
    if !waiting.is_empty() {
        d.nacked.lock().unwrap().insert(key.clone());
    }

    let (tx, mut rx) = mpsc::unbounded_channel();
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let already = {
        let mut map = LISTENERS.lock().unwrap();
        if a.keep_newest() {
            // One conversation open per Cursor process: an older listener of
            // the same process waits for a conversation it no longer shows.
            let stale: Vec<Key> = map
                .iter()
                .filter(|(k, l)| k.0 == tool && (**k == key || (pid.is_some() && l.pid == pid)))
                .map(|(k, _)| k.clone())
                .collect();
            for k in stale {
                if let Some(old) = map.remove(&k) {
                    let _ = old.tx.send(Event::End);
                }
            }
        }
        let already = !a.keep_newest() && map.get(&key).is_some_and(|l| !l.tx.is_closed());
        if !already {
            map.insert(key.clone(), Listener { id, pid, tx });
        }
        already
    };
    if already {
        reply(
            &mut w,
            &WaitReply {
                text: None,
                already: true,
            },
        )
        .await;
        return;
    }
    info!(tool, session = %session_id, "listener waiting");
    d.flush_presence();
    let since = Instant::now();

    let ours = |key: &Key| {
        let mut map = LISTENERS.lock().unwrap();
        if map.get(key).is_some_and(|l| l.id == id) {
            map.remove(key);
        }
    };
    tokio::select! {
        ev = rx.recv() => match ev {
            Some(Event::Hand(text, done)) => {
                let ok = reply(&mut w, &WaitReply { text: Some(text), already: false }).await
                    && confirmed(&mut lines).await;
                let _ = done.send(ok);
                record(&key, false);
                if !ok {
                    warn!(tool, session = %session_id, "listener ended before confirming");
                }
            }
            Some(Event::End) | None => {
                reply(&mut w, &WaitReply::default()).await;
                record(&key, false);
            }
        },
        // The listener went away: its tool ended it, or it crashed.
        _ = lines.next_line() => {
            ours(&key);
            record(&key, since.elapsed() < SHORT);
            info!(tool, session = %session_id, secs = since.elapsed().as_secs(), "listener left");
        }
    }
    d.flush_presence();
}

/// Messages the server holds for the session, left there (API-016); none
/// when the server can't be reached. Notice counts of messages no longer
/// waiting are dropped.
async fn waiting_on_server(d: &D, key: &Key) -> Vec<MessageView> {
    let actor = format!("{}:{}", key.0, key.1);
    let r = super::socket::call(d, "GET", "/v1/inbox", None, Some(actor)).await;
    if !r.ok {
        return vec![];
    }
    let messages: Vec<MessageView> = serde_json::from_value::<InboxResponse>(r.body)
        .map(|i| i.items.into_iter().map(|i| i.message).collect())
        .unwrap_or_default();
    if let Some(n) = NOTICES.lock().unwrap().get_mut(key) {
        n.retain(|id, _| messages.iter().any(|m| m.id == *id));
    }
    messages
}
