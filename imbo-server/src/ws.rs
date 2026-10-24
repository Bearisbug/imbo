//! API-023 `/v1/ws`: one connection per device, presence in, deliveries out.

use std::collections::HashSet;
use std::time::Duration;

use axum::Extension;
use axum::extract::State;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use futures::{SinkExt, StreamExt};
use imbo_proto::api::Account;
use imbo_proto::frames::{ClientFrame, PresenceItem, ServerFrame};
use imbo_proto::text::{clean_title, short_id};
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::auth::Caller;
use crate::delivery;
use crate::hub::Outbound;
use crate::state::St;

pub async fn upgrade(
    State(st): State<St>,
    Extension(caller): Extension<Caller>,
    ws: WebSocketUpgrade,
) -> Response {
    ws.on_upgrade(move |socket| run(st, caller, socket))
}

async fn run(st: St, caller: Caller, socket: WebSocket) {
    let device = caller.device_id;
    let account = caller.account_id;
    let (conn_id, mut rx) = st.hub.register(device, account);
    info!(%device, account, "device connected");
    let (mut sink, mut stream) = socket.split();

    let writer = tokio::spawn(async move {
        let mut ping = tokio::time::interval(Duration::from_secs(25));
        loop {
            tokio::select! {
                out = rx.recv() => match out {
                    Some(Outbound::Frame(f)) => {
                        let text = serde_json::to_string(&f).unwrap_or_default();
                        if sink.send(Message::Text(text.into())).await.is_err() { break; }
                    }
                    Some(Outbound::Close(code, reason)) => {
                        let _ = sink.send(Message::Close(Some(CloseFrame { code, reason: reason.into() }))).await;
                        break;
                    }
                    None => break,
                },
                _ = ping.tick() => {
                    if sink.send(Message::Ping(Vec::new().into())).await.is_err() { break; }
                }
            }
        }
    });

    while let Some(msg) = stream.next().await {
        let Ok(msg) = msg else { break };
        let text = match msg {
            Message::Text(t) => t,
            Message::Close(_) => break,
            _ => continue,
        };
        let frame: ClientFrame = match serde_json::from_str(&text) {
            Ok(f) => f,
            Err(e) => {
                warn!(%device, error = %e, "bad frame");
                continue;
            }
        };
        if !st.hub.is_current(device, conn_id) {
            break;
        }
        if let Err(e) = handle(&st, &caller, frame).await {
            warn!(%device, error = %e, "frame handling failed");
        }
    }

    writer.abort();
    if st.hub.unregister(device, conn_id) {
        // Connection gone: every session reported by this device is offline.
        let _ = sqlx::query(
            "UPDATE agents SET online = false, last_online_at = now() WHERE device_id = $1 AND online",
        )
        .bind(device)
        .execute(&st.db)
        .await;
        // Anything in flight comes back immediately for the next connection.
        let _ = sqlx::query(
            "UPDATE deliveries d SET state = 'pending', next_push_at = now()
             FROM agents a WHERE a.id = d.agent_id AND a.device_id = $1 AND d.state = 'pushed'",
        )
        .bind(device)
        .execute(&st.db)
        .await;
    }
    info!(%device, "device disconnected");
}

async fn handle(st: &St, caller: &Caller, frame: ClientFrame) -> anyhow::Result<()> {
    match frame {
        ClientFrame::Hello {
            client_version,
            sessions,
        } => {
            sqlx::query(
                "UPDATE devices SET client_version = $2, last_seen_at = now() WHERE id = $1",
            )
            .bind(caller.device_id)
            .bind(&client_version)
            .execute(&st.db)
            .await?;
            let email: String = sqlx::query_scalar("SELECT email FROM accounts WHERE id = $1")
                .bind(caller.account_id)
                .fetch_one(&st.db)
                .await?;
            st.hub.send(
                caller.device_id,
                ServerFrame::Welcome {
                    account: Account {
                        id: caller.account_id,
                        name: caller.name.clone(),
                        email: Some(email),
                    },
                    device_id: caller.device_id,
                    min_client_version: st.cfg.min_client_version.clone(),
                },
            );
            let online = apply_presence(st, caller, &sessions).await?;
            // Sessions this device no longer reports are offline.
            let keep: Vec<Uuid> = online.clone();
            sqlx::query(
                "UPDATE agents SET online = false, last_online_at = now()
                 WHERE device_id = $1 AND online AND NOT (id = ANY($2))",
            )
            .bind(caller.device_id)
            .bind(&keep)
            .execute(&st.db)
            .await?;
            delivery::kick(st, online);
        }
        ClientFrame::Presence { sessions } => {
            let online = apply_presence(st, caller, &sessions).await?;
            delivery::kick(st, online);
        }
        ClientFrame::Ack {
            message_id,
            agent_id,
            via,
        } => {
            let changed = delivery::ack(st, caller.account_id, message_id, agent_id, &via).await?;
            debug!(message_id, %agent_id, changed, "ack");
        }
        ClientFrame::Nack {
            message_id,
            agent_id,
            reason,
        } => {
            info!(message_id, %agent_id, reason, "nack");
            delivery::nack(st, caller.account_id, message_id, agent_id).await?;
        }
    }
    Ok(())
}

/// Upsert reported sessions; returns the ids of sessions now online.
async fn apply_presence(
    st: &St,
    caller: &Caller,
    items: &[PresenceItem],
) -> anyhow::Result<Vec<Uuid>> {
    let mut online = vec![];
    let mut seen = HashSet::new();
    for item in items {
        if item.session_id.is_empty()
            || item.session_id.len() > 128
            || item.tool.is_empty()
            || item.tool.len() > 32
        {
            continue;
        }
        if !seen.insert((item.tool.clone(), item.session_id.clone())) {
            continue;
        }
        let title = clean_title(&item.title, &item.session_id);
        let id: Uuid = sqlx::query_scalar(
            "INSERT INTO agents (id, account_id, device_id, tool, session_id, short_id, title, online, last_online_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, now())
             ON CONFLICT (account_id, tool, session_id) DO UPDATE SET
               device_id = CASE WHEN EXCLUDED.online OR agents.device_id = EXCLUDED.device_id
                                THEN EXCLUDED.device_id ELSE agents.device_id END,
               title = EXCLUDED.title,
               online = CASE WHEN EXCLUDED.online THEN true
                             WHEN agents.device_id = EXCLUDED.device_id THEN false
                             ELSE agents.online END,
               last_online_at = now()
             RETURNING id",
        )
        .bind(Uuid::now_v7())
        .bind(caller.account_id)
        .bind(caller.device_id)
        .bind(&item.tool)
        .bind(&item.session_id)
        .bind(short_id(&item.session_id))
        .bind(&title)
        .bind(item.online)
        .fetch_one(&st.db)
        .await?;
        if item.online {
            online.push(id);
        }
    }
    Ok(online)
}
