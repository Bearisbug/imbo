//! API-058 blockAccount, API-059 unblockAccount, API-060 listBlocks,
//! API-061 createReport. Only the web app and the App reach these (DESIGN §10).

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::{Extension, Json};
use chrono::{DateTime, Utc};
use imbo_proto::ErrorCode;
use imbo_proto::api::{
    BlockItem, BlockRequest, BlockResponse, BlocksResponse, Profile, ReportCreated,
    ReportCreatedResponse, ReportRequest,
};
use serde_json::{Value, json};
use sqlx::PgConnection;
use tracing::{info, warn};
use uuid::Uuid;

use super::Body;
use crate::auth::Caller;
use crate::error::{ApiResult, err};
use crate::state::St;
use crate::{delivery, safety, views};

const REASONS: [&str; 4] = ["spam", "harassment", "inappropriate_avatar", "other"];
const MAX_REPORT_NOTE: usize = 500;
const REPORTS_PER_DAY: i64 = 10;
/// Messages copied into a report (ADR-039).
const SNAPSHOT_MESSAGES: i64 = 20;

async fn block_item(st: &St, account: i32, created_at: DateTime<Utc>) -> ApiResult<BlockItem> {
    let name: String = sqlx::query_scalar("SELECT name FROM accounts WHERE id = $1")
        .bind(account)
        .fetch_one(&st.db)
        .await?;
    let avatar_url = safety::avatars(&st.db, &st.cfg, &[account])
        .await?
        .remove(&account);
    Ok(BlockItem {
        account: Profile {
            id: account,
            name,
            avatar_url,
        },
        created_at,
    })
}

/// API-058: block an account, contact or not (ADR-038).
pub async fn block(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
    Body(req): Body<BlockRequest>,
) -> ApiResult<(StatusCode, Json<BlockResponse>)> {
    let target = req.account_id;
    if target == c.account_id {
        return Err(err(ErrorCode::InvalidRequest, "You can't block yourself."));
    }
    if views::account_ref(&st.db, target).await?.is_none() {
        return Err(err(
            ErrorCode::AccountNotFound,
            format!("No account with ID {target}."),
        ));
    }
    let me = c.account_id;
    let mut tx = st.db.begin().await?;
    // Their sends lock their own row for share: one in flight finishes
    // first and is taken back below; later ones see the block (DESIGN §16).
    sqlx::query("SELECT 1 FROM accounts WHERE id = $1 FOR NO KEY UPDATE")
        .bind(target)
        .execute(&mut *tx)
        .await?;
    // Their deletion committed while we waited: nothing to block. Checked in
    // its own statement, which sees what committed during the wait.
    let gone: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM deleted_accounts WHERE id = $1)")
            .bind(target)
            .fetch_one(&mut *tx)
            .await?;
    if gone {
        return Err(err(
            ErrorCode::AccountNotFound,
            format!("No account with ID {target}."),
        ));
    }
    let created: Option<DateTime<Utc>> = sqlx::query_scalar(
        "INSERT INTO blocks (account_id, blocked_id) VALUES ($1, $2)
         ON CONFLICT DO NOTHING RETURNING created_at",
    )
    .bind(me)
    .bind(target)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(created_at) = created else {
        let at: DateTime<Utc> = sqlx::query_scalar(
            "SELECT created_at FROM blocks WHERE account_id = $1 AND blocked_id = $2",
        )
        .bind(me)
        .bind(target)
        .fetch_one(&mut *tx)
        .await?;
        tx.commit().await?;
        let block = block_item(&st, target, at).await?;
        return Ok((StatusCode::OK, Json(BlockResponse { block })));
    };
    let canceled = take_back(&mut tx, me, target).await?;
    // Pending contact requests and group invites between the two, either
    // way, close without a notice (ENT-Request pending → declined).
    sqlx::query(
        "UPDATE requests SET status = 'declined', decided_at = now()
         WHERE status = 'pending'
           AND ((from_account = $1 AND to_account = $2) OR (from_account = $2 AND to_account = $1))",
    )
    .bind(me)
    .bind(target)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    delivery::notify_canceled(&st, canceled).await;
    info!(
        event = "blocked_account",
        account_id = me,
        blocked_id = target,
        "blocked"
    );
    let block = block_item(&st, target, created_at).await?;
    Ok((StatusCode::CREATED, Json(BlockResponse { block })))
}

/// Undelivered direct messages between the two, both ways, and the
/// blocked account's undelivered group messages to me.
async fn take_back(
    tx: &mut PgConnection,
    me: i32,
    target: i32,
) -> Result<Vec<(i64, Uuid)>, sqlx::Error> {
    let (lo, hi) = (me.min(target), me.max(target));
    sqlx::query_as(
        "UPDATE deliveries d SET state = 'canceled', canceled_at = now(), cancel_reason = 'blocked'
         FROM messages m JOIN conversations cv ON cv.id = m.conversation_id
         WHERE d.message_id = m.id AND d.state IN ('pending', 'pushed')
           AND ((cv.kind = 'direct' AND cv.account_lo = $1 AND cv.account_hi = $2)
                OR (cv.kind = 'group' AND m.from_account = $4 AND d.account_id = $3))
         RETURNING d.message_id, d.agent_id",
    )
    .bind(lo)
    .bind(hi)
    .bind(me)
    .bind(target)
    .fetch_all(&mut *tx)
    .await
}

/// API-059: what was refused or taken back meanwhile stays that way.
pub async fn unblock(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
    Path(target): Path<String>,
) -> ApiResult<StatusCode> {
    let missing = || err(ErrorCode::NotFound, "You haven't blocked that account.");
    let target: i32 = target.parse().map_err(|_| missing())?;
    let r = sqlx::query("DELETE FROM blocks WHERE account_id = $1 AND blocked_id = $2")
        .bind(c.account_id)
        .bind(target)
        .execute(&st.db)
        .await?;
    if r.rows_affected() == 0 {
        return Err(missing());
    }
    Ok(StatusCode::NO_CONTENT)
}

/// API-060: newest block first.
pub async fn list(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
) -> ApiResult<Json<BlocksResponse>> {
    let rows: Vec<(i32, String, DateTime<Utc>)> = sqlx::query_as(
        "SELECT a.id, a.name, b.created_at FROM blocks b JOIN accounts a ON a.id = b.blocked_id
         WHERE b.account_id = $1 ORDER BY b.created_at DESC, a.id",
    )
    .bind(c.account_id)
    .fetch_all(&st.db)
    .await?;
    let ids: Vec<i32> = rows.iter().map(|r| r.0).collect();
    let mut avatars = safety::avatars(&st.db, &st.cfg, &ids).await?;
    let blocks = rows
        .into_iter()
        .map(|(id, name, created_at)| BlockItem {
            account: Profile {
                id,
                name,
                avatar_url: avatars.remove(&id),
            },
            created_at,
        })
        .collect();
    Ok(Json(BlocksResponse { blocks }))
}

/// One copied message: who sent it is kept as the account ID and named
/// when the report is read, so a deleted sender shows as Deleted account.
fn copied(kind: &str, from: i32, body: &str, files: Value, at: DateTime<Utc>) -> Value {
    json!({ "kind": kind, "from": from, "body": body, "files": files, "created_at": at })
}

/// Copy the last 20 messages into the report, files as name and size only;
/// in a direct report the notes of their pending requests to me count among
/// the 20.
async fn snapshot(
    tx: &mut PgConnection,
    me: i32,
    target: i32,
    group: Option<i32>,
) -> Result<Value, sqlx::Error> {
    let rows: Vec<(i64, i32, String, DateTime<Utc>)> = match group {
        None => {
            let (lo, hi) = (me.min(target), me.max(target));
            sqlx::query_as(
                "SELECT m.id, m.from_account, m.body, m.created_at FROM messages m
                 JOIN conversations cv ON cv.id = m.conversation_id
                 WHERE cv.kind = 'direct' AND cv.account_lo = $1 AND cv.account_hi = $2
                 ORDER BY m.id DESC LIMIT $3",
            )
            .bind(lo)
            .bind(hi)
            .bind(SNAPSHOT_MESSAGES)
            .fetch_all(&mut *tx)
            .await?
        }
        Some(gid) => {
            sqlx::query_as(
                "SELECT m.id, m.from_account, m.body, m.created_at FROM messages m
                 JOIN conversations cv ON cv.id = m.conversation_id
                 WHERE cv.group_id = $1 AND m.from_account = $2
                 ORDER BY m.id DESC LIMIT $3",
            )
            .bind(gid)
            .bind(target)
            .bind(SNAPSHOT_MESSAGES)
            .fetch_all(&mut *tx)
            .await?
        }
    };
    let ids: Vec<i64> = rows.iter().map(|r| r.0).collect();
    let files: Vec<(i64, String, i64)> = sqlx::query_as(
        "SELECT message_id, name, size FROM files WHERE message_id = ANY($1) ORDER BY created_at, id",
    )
    .bind(&ids)
    .fetch_all(&mut *tx)
    .await?;
    let mut out: Vec<(DateTime<Utc>, Value)> = vec![];
    if group.is_none() {
        let notes: Vec<(String, DateTime<Utc>)> = sqlx::query_as(
            "SELECT note, created_at FROM requests
             WHERE from_account = $1 AND to_account = $2 AND status = 'pending'
               AND note IS NOT NULL
             ORDER BY id DESC LIMIT $3",
        )
        .bind(target)
        .bind(me)
        .bind(SNAPSHOT_MESSAGES)
        .fetch_all(&mut *tx)
        .await?;
        for (note, at) in notes.into_iter().rev() {
            out.push((at, copied("request_note", target, &note, json!([]), at)));
        }
    }
    for (id, from, body, at) in rows.into_iter().rev() {
        let list: Vec<Value> = files
            .iter()
            .filter(|f| f.0 == id)
            .map(|f| json!({ "name": f.1, "size": f.2 }))
            .collect();
        out.push((at, copied("message", from, &body, Value::Array(list), at)));
    }
    // Oldest first, the newest 20 of messages and notes together.
    out.sort_by_key(|(at, _)| *at);
    let skip = out.len().saturating_sub(SNAPSHOT_MESSAGES as usize);
    Ok(Value::Array(
        out.into_iter().skip(skip).map(|(_, v)| v).collect(),
    ))
}

/// API-061: store the report with the recent messages, then mail the
/// report address; a mail failure is only logged (ADR-039).
pub async fn report(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
    Body(req): Body<ReportRequest>,
) -> ApiResult<(StatusCode, Json<ReportCreatedResponse>)> {
    let me = c.account_id;
    let target = req.account_id;
    if target == me {
        return Err(err(ErrorCode::InvalidRequest, "You can't report yourself."));
    }
    if !REASONS.contains(&req.reason.as_str()) {
        return Err(err(
            ErrorCode::InvalidRequest,
            "reason must be spam, harassment, inappropriate_avatar or other.",
        ));
    }
    let note = req
        .note
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty());
    if note
        .as_ref()
        .is_some_and(|n| n.chars().count() > MAX_REPORT_NOTE)
    {
        return Err(err(
            ErrorCode::InvalidRequest,
            "The note can be at most 500 characters.",
        ));
    }
    if views::account_ref(&st.db, target).await?.is_none() {
        return Err(err(
            ErrorCode::AccountNotFound,
            format!("No account with ID {target}."),
        ));
    }
    if let Some(gid) = req.group_id {
        let row: Option<(String, bool)> = sqlx::query_as(
            "SELECT g.name, EXISTS (SELECT 1 FROM group_members m WHERE m.group_id = g.id AND m.account_id = $2)
             FROM chat_groups g WHERE g.id = $1 AND g.dissolved_at IS NULL",
        )
        .bind(gid)
        .bind(me)
        .fetch_optional(&st.db)
        .await?;
        match row {
            None => return Err(err(ErrorCode::NotFound, format!("No group #{gid}."))),
            Some((name, false)) => {
                return Err(err(
                    ErrorCode::NotMember,
                    format!("You are not in group {name} (#{gid})."),
                ));
            }
            Some(_) => {}
        }
    }

    let mut tx = st.db.begin().await?;
    let open: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM reports WHERE reporter_id = $1 AND target_id = $2 AND status = 'open')",
    )
    .bind(me)
    .bind(target)
    .fetch_one(&mut *tx)
    .await?;
    if open {
        return Err(already_reported());
    }
    let today: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM reports WHERE reporter_id = $1 AND created_at > now() - interval '1 day'",
    )
    .bind(me)
    .fetch_one(&mut *tx)
    .await?;
    if today >= REPORTS_PER_DAY {
        return Err(err(
            ErrorCode::RateLimited,
            "At most 10 reports a day. Try again tomorrow.",
        )
        .retry_after(3600));
    }
    let copy = snapshot(&mut tx, me, target, req.group_id).await?;
    // Two at once: the partial unique index keeps one (INV-031).
    let inserted: Result<(i64, DateTime<Utc>), sqlx::Error> = sqlx::query_as(
        "INSERT INTO reports (reporter_id, target_id, group_id, reason, note, snapshot)
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING id, created_at",
    )
    .bind(me)
    .bind(target)
    .bind(req.group_id)
    .bind(&req.reason)
    .bind(&note)
    .bind(&copy)
    .fetch_one(&mut *tx)
    .await;
    let (id, created_at) = match inserted {
        Ok(row) => row,
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
            return Err(already_reported());
        }
        Err(e) => return Err(e.into()),
    };
    tx.commit().await?;
    info!(
        event = "report_created",
        report_id = id,
        reason = %req.reason,
        "report stored"
    );
    tokio::spawn(async move {
        if let Err(e) = st.mailer.send_report(&st.cfg, id).await {
            warn!(report_id = id, error = %e, "report mail not sent");
        }
    });
    Ok((
        StatusCode::CREATED,
        Json(ReportCreatedResponse {
            report: ReportCreated { id, created_at },
        }),
    ))
}

fn already_reported() -> crate::error::ApiError {
    err(
        ErrorCode::AlreadyReported,
        "You already reported this account. It will be handled within 24 hours.",
    )
}
