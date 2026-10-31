//! Delivery state machine (ENT-Delivery): push, requeue, cancel.

use std::time::Duration;

use chrono::{DateTime, Utc};
use imbo_proto::frames::{Delivery, ServerFrame};
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::state::St;
use crate::views;

/// SQL for the next push time after `attempts` failed pushes:
/// 30 s, 1, 2, 5, then every 10 minutes.
pub const BACKOFF_SQL: &str = "now() + CASE
    WHEN attempts <= 1 THEN interval '30 seconds'
    WHEN attempts = 2 THEN interval '1 minute'
    WHEN attempts = 3 THEN interval '2 minutes'
    WHEN attempts = 4 THEN interval '5 minutes'
    ELSE interval '10 minutes' END";

pub fn spawn_tasks(st: St) {
    {
        let st = st.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(5));
            loop {
                tick.tick().await;
                if let Err(e) = requeue(&st).await {
                    warn!(error = %e, "delivery.requeue");
                }
                if let Err(e) = push_due(&st).await {
                    warn!(error = %e, "delivery.push");
                }
            }
        });
    }
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(Duration::from_secs(3600));
        loop {
            tick.tick().await;
            match sqlx::query("DELETE FROM email_codes WHERE expires_at < now() - interval '1 day'")
                .execute(&st.db)
                .await
            {
                Ok(r) if r.rows_affected() > 0 => {
                    info!(deleted = r.rows_affected(), "email_code.cleanup")
                }
                Ok(_) => {}
                Err(e) => warn!(error = %e, "email_code.cleanup"),
            }
            if let Err(e) = file_cleanup(&st).await {
                warn!(error = %e, "file.cleanup");
            }
            // Reports closed a year ago go, with what was copied into them (ADR-039).
            match sqlx::query(
                "DELETE FROM reports WHERE status <> 'open' AND resolved_at < now() - interval '1 year'",
            )
            .execute(&st.db)
            .await
            {
                Ok(r) if r.rows_affected() > 0 => {
                    info!(deleted = r.rows_affected(), "report.cleanup")
                }
                Ok(_) => {}
                Err(e) => warn!(error = %e, "report.cleanup"),
            }
        }
    });
}

/// How long a pushed delivery waits for its ack: 30 s, plus 4 s per MB of
/// attachments, which the receiving computer downloads first (ADR-012).
pub fn ack_timeout_secs(file_bytes: i64) -> f64 {
    30.0 + 4.0 * file_bytes.max(0) as f64 / 1_048_576.0
}

/// pushed → pending when no ack within the ack timeout.
async fn requeue(st: &St) -> Result<(), sqlx::Error> {
    let rows: Vec<(i64, Uuid, f64, i64)> = sqlx::query_as(
        "SELECT d.message_id, d.agent_id, extract(epoch FROM now() - d.pushed_at)::float8,
                coalesce((SELECT sum(f.size) FROM files f WHERE f.message_id = d.message_id), 0)::bigint
         FROM deliveries d WHERE d.state = 'pushed' AND d.pushed_at < now() - interval '30 seconds'",
    )
    .fetch_all(&st.db)
    .await?;
    let (messages, agents): (Vec<i64>, Vec<Uuid>) = rows
        .into_iter()
        .filter(|(_, _, age, bytes)| *age > ack_timeout_secs(*bytes))
        .map(|(m, a, _, _)| (m, a))
        .unzip();
    if messages.is_empty() {
        return Ok(());
    }
    let sql = format!(
        "UPDATE deliveries SET state = 'pending', next_push_at = {BACKOFF_SQL}
         FROM unnest($1::bigint[], $2::uuid[]) AS t(message_id, agent_id)
         WHERE deliveries.message_id = t.message_id AND deliveries.agent_id = t.agent_id
           AND deliveries.state = 'pushed'"
    );
    let r = sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(&messages)
        .bind(&agents)
        .execute(&st.db)
        .await?;
    if r.rows_affected() > 0 {
        info!(count = r.rows_affected(), "delivery.requeue: ack timed out");
    }
    Ok(())
}

/// `file.cleanup`: content of files not sent within the hour, and of sent
/// files past their 7 days, is deleted; rows stay so the day's upload total
/// stays right (ENT-File).
pub async fn file_cleanup(st: &St) -> Result<(), sqlx::Error> {
    let gone: Vec<Uuid> = sqlx::query_scalar(
        "UPDATE files SET state = CASE state WHEN 'ready' THEN 'discarded' ELSE 'expired' END
         WHERE (state = 'ready' AND created_at < now() - interval '1 hour')
            OR (state = 'sent' AND expires_at < now())
         RETURNING id",
    )
    .fetch_all(&st.db)
    .await?;
    // Uploads cut off by a server restart.
    let stale: Vec<Uuid> = sqlx::query_scalar(
        "DELETE FROM files WHERE state = 'uploading' AND created_at < now() - interval '6 hours'
         RETURNING id",
    )
    .fetch_all(&st.db)
    .await?;
    let dir = st.files_dir();
    for id in &gone {
        let _ = tokio::fs::remove_file(dir.join(id.to_string())).await;
    }
    for id in &stale {
        let _ = tokio::fs::remove_file(dir.join(format!("{id}.part"))).await;
    }
    if !gone.is_empty() || !stale.is_empty() {
        info!(
            removed = gone.len(),
            stale_uploads = stale.len(),
            "file.cleanup"
        );
    }
    Ok(())
}

async fn push_due(st: &St) -> Result<(), sqlx::Error> {
    let agents: Vec<Uuid> = sqlx::query_scalar(
        "SELECT DISTINCT d.agent_id FROM deliveries d JOIN agents a ON a.id = d.agent_id
         WHERE d.state = 'pending' AND d.next_push_at <= now() AND a.online",
    )
    .fetch_all(&st.db)
    .await?;
    for agent in agents {
        push_agent(st, agent).await?;
    }
    Ok(())
}

/// Push in the background (after a send or when a session comes online).
pub fn kick(st: &St, agents: impl IntoIterator<Item = Uuid>) {
    for agent in agents {
        let st = st.clone();
        tokio::spawn(async move {
            if let Err(e) = push_agent(&st, agent).await {
                warn!(error = %e, %agent, "delivery.push");
            }
        });
    }
}

/// pending → pushed for one online agent, in message order. Stops at the
/// first delivery that is still backing off so later messages don't overtake it.
pub async fn push_agent(st: &St, agent: Uuid) -> Result<(), sqlx::Error> {
    let lock = st.push_lock(agent);
    let _guard = lock.lock().await;
    let row: Option<(Option<Uuid>, String, String, bool, i32)> = sqlx::query_as(
        "SELECT device_id, tool, session_id, online, account_id FROM agents WHERE id = $1",
    )
    .bind(agent)
    .fetch_optional(&st.db)
    .await?;
    let Some((Some(device), tool, session_id, true, account_id)) = row else {
        return Ok(());
    };
    if !st.hub.connected(device) {
        return Ok(());
    }
    let pending: Vec<(i64, DateTime<Utc>)> = sqlx::query_as(
        "SELECT message_id, next_push_at FROM deliveries
         WHERE agent_id = $1 AND state = 'pending' ORDER BY message_id LIMIT 50",
    )
    .bind(agent)
    .fetch_all(&st.db)
    .await?;
    let now = Utc::now();
    let due: Vec<i64> = pending
        .iter()
        .take_while(|(_, at)| *at <= now)
        .map(|(id, _)| *id)
        .collect();
    if due.is_empty() {
        return Ok(());
    }
    let pushed: Vec<i64> = sqlx::query_scalar(
        "UPDATE deliveries SET state = 'pushed', attempts = attempts + 1, pushed_at = now()
         WHERE agent_id = $1 AND message_id = ANY($2) AND state = 'pending'
         RETURNING message_id",
    )
    .bind(agent)
    .bind(&due)
    .fetch_all(&st.db)
    .await?;
    if pushed.is_empty() {
        return Ok(());
    }
    let mut views = views::messages(&st.db, &pushed, account_id).await?;
    views::with_siblings(&st.db, &mut views, account_id, agent).await?;
    let mut failed = vec![];
    for message in views {
        let id = message.id;
        let frame = ServerFrame::Deliver(Delivery {
            agent_id: agent,
            tool: tool.clone(),
            session_id: session_id.clone(),
            message,
        });
        if st.hub.send(device, frame) {
            debug!(message = id, %agent, "pushed");
        } else {
            failed.push(id);
        }
    }
    if !failed.is_empty() {
        sqlx::query(
            "UPDATE deliveries SET state = 'pending', next_push_at = now()
             WHERE agent_id = $1 AND message_id = ANY($2) AND state = 'pushed'",
        )
        .bind(agent)
        .bind(&failed)
        .execute(&st.db)
        .await?;
    }
    Ok(())
}

/// ack: pending|pushed → delivered. Returns false when the delivery was not
/// waiting (already delivered or canceled), which is fine.
pub async fn ack(
    st: &St,
    account_id: i32,
    message_id: i64,
    agent: Uuid,
    via: &str,
) -> Result<bool, sqlx::Error> {
    let via = match via {
        "prompt_hook" | "pull" => via,
        _ => "inject",
    };
    let r = sqlx::query(
        "UPDATE deliveries SET state = 'delivered', delivered_at = now(), via = $4
         WHERE message_id = $1 AND agent_id = $2 AND account_id = $3 AND state IN ('pending', 'pushed')",
    )
    .bind(message_id)
    .bind(agent)
    .bind(account_id)
    .bind(via)
    .execute(&st.db)
    .await?;
    Ok(r.rows_affected() > 0)
}

/// nack: pushed → pending with backoff.
pub async fn nack(
    st: &St,
    account_id: i32,
    message_id: i64,
    agent: Uuid,
) -> Result<(), sqlx::Error> {
    let sql = format!(
        "UPDATE deliveries SET state = 'pending', next_push_at = {BACKOFF_SQL}
         WHERE message_id = $1 AND agent_id = $2 AND account_id = $3 AND state = 'pushed'"
    );
    sqlx::query(sqlx::AssertSqlSafe(sql))
        .bind(message_id)
        .bind(agent)
        .bind(account_id)
        .execute(&st.db)
        .await?;
    Ok(())
}

/// Send `cancel` frames for deliveries that were canceled while pushed.
pub async fn notify_canceled(st: &St, rows: Vec<(i64, Uuid)>) {
    for (message_id, agent_id) in rows {
        let device: Option<Option<Uuid>> =
            sqlx::query_scalar("SELECT device_id FROM agents WHERE id = $1")
                .bind(agent_id)
                .fetch_optional(&st.db)
                .await
                .unwrap_or(None);
        if let Some(Some(device)) = device {
            st.hub.send(
                device,
                ServerFrame::Cancel {
                    message_id,
                    agent_id,
                },
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ack_timeout_grows_with_attachments() {
        assert_eq!(ack_timeout_secs(0), 30.0);
        assert_eq!(ack_timeout_secs(1_048_576), 34.0);
        // 100 MB: 430 s, about 7 minutes.
        assert_eq!(ack_timeout_secs(104_857_600), 430.0);
    }
}
