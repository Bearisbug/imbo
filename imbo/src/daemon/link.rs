//! The device WebSocket (API-023): hello, presence, deliveries, reconnects.

use std::time::Duration;

use futures::{SinkExt, StreamExt};
use imbo_proto::frames::{CLOSE_UNAUTHORIZED, ClientFrame, ServerFrame};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::Message;
use tracing::{info, warn};

use super::D;
use crate::net::{WsError, ws_connect};

pub async fn run(d: D) {
    let mut backoff = 1u64;
    loop {
        let Some(creds) = d.creds() else {
            {
                let mut l = d.link.lock().unwrap();
                l.connected = false;
                l.last_error = Some("not logged in".into());
            }
            d.reload.notified().await;
            continue;
        };
        let stopped = {
            let l = d.link.lock().unwrap();
            l.revoked || l.outdated.is_some()
        };
        if stopped {
            d.reload.notified().await;
            continue;
        }
        match ws_connect(&creds.server, &creds.token).await {
            Err(WsError::Unauthorized) => {
                warn!("the server rejected this computer's token; run `imbo login`");
                {
                    let mut l = d.link.lock().unwrap();
                    l.revoked = true;
                    l.connected = false;
                    l.last_error =
                        Some("this computer's sign-in was revoked; run `imbo login`".into());
                }
                continue;
            }
            Err(WsError::Other(e)) => {
                {
                    let mut l = d.link.lock().unwrap();
                    l.connected = false;
                    l.last_error = Some(format!("cannot reach {}: {e}", creds.server));
                }
                warn!(server = %creds.server, error = %e, retry_in = backoff, "connect failed");
                tokio::select! {
                    _ = tokio::time::sleep(Duration::from_secs(backoff)) => {}
                    _ = d.reload.notified() => {}
                }
                backoff = (backoff * 2).min(60);
                continue;
            }
            Ok(ws) => {
                backoff = 1;
                session(&d, ws).await;
                d.link.lock().unwrap().connected = false;
                *d.out.lock().unwrap() = None;
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
        }
    }
}

async fn session(d: &D, ws: crate::net::Ws) {
    let (mut sink, mut stream) = ws.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<ClientFrame>();
    let hello = d.sessions.lock().unwrap().hello();
    let n = hello.len();
    let frame = ClientFrame::Hello {
        client_version: crate::VERSION.into(),
        sessions: hello,
    };
    if sink
        .send(Message::Text(
            serde_json::to_string(&frame).unwrap_or_default().into(),
        ))
        .await
        .is_err()
    {
        return;
    }
    *d.out.lock().unwrap() = Some(tx);
    {
        let mut l = d.link.lock().unwrap();
        l.connected = true;
        l.last_error = None;
    }
    info!(sessions = n, "connected");
    d.flush_presence();

    let mut last_seen = tokio::time::Instant::now();
    let mut check = tokio::time::interval(Duration::from_secs(15));
    loop {
        tokio::select! {
            out = rx.recv() => {
                let Some(f) = out else { break };
                let text = serde_json::to_string(&f).unwrap_or_default();
                if sink.send(Message::Text(text.into())).await.is_err() { break; }
            }
            msg = stream.next() => {
                let Some(Ok(msg)) = msg else {
                    warn!("connection lost");
                    break;
                };
                last_seen = tokio::time::Instant::now();
                match msg {
                    Message::Text(t) => match serde_json::from_str::<ServerFrame>(&t) {
                        Ok(f) => if !handle(d, f) { break; },
                        Err(e) => warn!(error = %e, "unknown frame"),
                    },
                    Message::Ping(p) => { let _ = sink.send(Message::Pong(p)).await; }
                    Message::Close(c) => {
                        let code: u16 = c.as_ref().map(|c| c.code.into()).unwrap_or(1000);
                        info!(code, "server closed the connection");
                        if code == CLOSE_UNAUTHORIZED {
                            let mut l = d.link.lock().unwrap();
                            l.revoked = true;
                            l.last_error = Some("this computer's sign-in was revoked; run `imbo login`".into());
                        }
                        break;
                    }
                    _ => {}
                }
            }
            _ = check.tick() => {
                if last_seen.elapsed() > Duration::from_secs(70) {
                    warn!("no traffic from the server for 70 s; reconnecting");
                    break;
                }
            }
            _ = d.reload.notified() => {
                info!("credentials changed; reconnecting");
                break;
            }
        }
    }
}

/// Returns false when the connection should end.
fn handle(d: &D, f: ServerFrame) -> bool {
    match f {
        ServerFrame::Welcome {
            account,
            device_id,
            min_client_version,
        } => {
            info!(account = account.id, name = %account.name, %device_id, "welcome");
            if crate::update::newer(&min_client_version, crate::VERSION) {
                // REQ-048: stop until the program is updated or signed in again.
                warn!(min = %min_client_version, "this server needs a newer imbo; run `imbo update`");
                let mut l = d.link.lock().unwrap();
                l.outdated = Some(min_client_version.clone());
                l.last_error = Some(format!(
                    "this server needs imbo {min_client_version} or newer; run `imbo update`"
                ));
                return false;
            }
        }
        ServerFrame::Deliver(delivery) => {
            info!(msg = delivery.message.id, tool = %delivery.tool, session = %delivery.session_id, "deliver");
            d.enqueue(delivery);
        }
        ServerFrame::Cancel {
            message_id,
            agent_id,
        } => {
            info!(msg = message_id, %agent_id, "cancel");
            d.canceled.lock().unwrap().insert((message_id, agent_id));
        }
        ServerFrame::Notice(n) => d.push_notice(n),
    }
    true
}
