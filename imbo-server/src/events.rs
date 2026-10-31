//! FLOW-LIVE: API-079 `/v1/events` tells the web app and the App which list
//! changed, so they fetch it again. Changes come from the database's
//! `imbo_events` notifications (migration 0013, ADR-058).

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use axum::Extension;
use axum::extract::State;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use futures::{SinkExt, StreamExt};
use tokio::sync::mpsc;
use tracing::warn;

use crate::auth::Caller;
use crate::state::St;

const CHANNEL: &str = "imbo_events";
const AREAS: &[&str] = &["conversations", "agents", "contacts", "requests", "groups"];
/// Sent when notifications may have been missed: fetch everything.
const ALL: &str = "all";
/// A ping keeps proxies from closing the connection; each one also checks
/// the sign-in is still good.
const PING: Duration = Duration::from_secs(30);

#[derive(Default)]
pub struct Events {
    conns: Mutex<HashMap<u64, (i32, mpsc::UnboundedSender<&'static str>)>>,
    next: AtomicU64,
}

impl Events {
    fn register(&self, account_id: i32) -> (u64, mpsc::UnboundedReceiver<&'static str>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        self.conns.lock().unwrap().insert(id, (account_id, tx));
        (id, rx)
    }

    fn unregister(&self, id: u64) {
        self.conns.lock().unwrap().remove(&id);
    }

    /// Tell every connection of the account that `area` changed.
    pub fn send(&self, account_id: i32, area: &'static str) {
        for (account, tx) in self.conns.lock().unwrap().values() {
            if *account == account_id {
                let _ = tx.send(area);
            }
        }
    }

    fn send_all(&self, area: &'static str) {
        for (_, tx) in self.conns.lock().unwrap().values() {
            let _ = tx.send(area);
        }
    }
}

/// `<account id>:<area>` from the database.
fn parse(payload: &str) -> Option<(i32, &'static str)> {
    let (account, area) = payload.split_once(':')?;
    Some((account.parse().ok()?, AREAS.iter().find(|a| **a == area)?))
}

pub fn spawn_listener(st: St) {
    tokio::spawn(async move {
        loop {
            if let Err(e) = listen(&st).await {
                warn!(error = %e, "events listener");
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
        }
    });
}

/// Returns when the connection to the database drops; listening again
/// starts with `all`, since changes in between were not heard.
async fn listen(st: &St) -> Result<(), sqlx::Error> {
    let mut listener = sqlx::postgres::PgListener::connect_with(&st.db).await?;
    listener.listen(CHANNEL).await?;
    st.events.send_all(ALL);
    while let Some(n) = listener.try_recv().await? {
        if let Some((account, area)) = parse(n.payload()) {
            st.events.send(account, area);
        }
    }
    Ok(())
}

/// Only web and App sign-ins get here (auth::WEB_ONLY); a web sign-in's
/// `Origin` is checked like a write's (ADR-023).
pub async fn upgrade(
    State(st): State<St>,
    Extension(caller): Extension<Caller>,
    ws: WebSocketUpgrade,
) -> Response {
    ws.on_upgrade(move |socket| run(st, caller, socket))
}

async fn run(st: St, caller: Caller, socket: WebSocket) {
    let (id, mut rx) = st.events.register(caller.account_id);
    let (mut sink, mut stream) = socket.split();
    let mut ping = tokio::time::interval_at(tokio::time::Instant::now() + PING, PING);
    loop {
        tokio::select! {
            area = rx.recv() => {
                let Some(area) = area else { break };
                let text = serde_json::json!({ "type": "changed", "area": area }).to_string();
                if sink.send(Message::Text(text.into())).await.is_err() { break; }
            }
            msg = stream.next() => match msg {
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(_)) => {}
            },
            _ = ping.tick() => {
                if !signed_in(&st, &caller).await {
                    let _ = sink.send(Message::Close(Some(CloseFrame {
                        code: imbo_proto::frames::CLOSE_UNAUTHORIZED,
                        reason: "signed out".into(),
                    }))).await;
                    break;
                }
                if sink.send(Message::Ping(Vec::new().into())).await.is_err() { break; }
            }
        }
    }
    st.events.unregister(id);
}

/// Signing out, a ban and deleting the account all revoke the sign-in.
async fn signed_in(st: &St, caller: &Caller) -> bool {
    let (sql, id) = match (caller.web, caller.app) {
        (Some(w), _) => (
            "SELECT EXISTS (SELECT 1 FROM web_sessions WHERE id = $1 AND revoked_at IS NULL AND expires_at > now())",
            w,
        ),
        (_, Some(a)) => (
            "SELECT EXISTS (SELECT 1 FROM app_sessions WHERE id = $1 AND revoked_at IS NULL AND expires_at > now())",
            a,
        ),
        _ => return false,
    };
    sqlx::query_scalar::<_, bool>(sql)
        .bind(id)
        .fetch_one(&st.db)
        .await
        .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_known_areas_only() {
        assert_eq!(parse("12345678:groups"), Some((12345678, "groups")));
        assert_eq!(parse("12345678:all"), None);
        assert_eq!(parse("x:groups"), None);
        assert_eq!(parse("12345678"), None);
    }

    #[test]
    fn sends_to_the_accounts_connections() {
        let e = Events::default();
        let (a1, mut r1) = e.register(1);
        let (_, mut r2) = e.register(1);
        let (_, mut r3) = e.register(2);
        e.send(1, "agents");
        assert_eq!(r1.try_recv().ok(), Some("agents"));
        assert_eq!(r2.try_recv().ok(), Some("agents"));
        assert!(r3.try_recv().is_err());
        e.unregister(a1);
        e.send_all(ALL);
        assert!(r1.try_recv().is_err());
        assert_eq!(r2.try_recv().ok(), Some(ALL));
        assert_eq!(r3.try_recv().ok(), Some(ALL));
    }
}
