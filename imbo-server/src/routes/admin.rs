//! The admin endpoints API-062–070, for ROLE-Admin only: a web sign-in
//! whose email is in `IMBO_ADMIN_EMAILS`, not banned (ADR-040). Anyone else
//! signed in gets 404, as if they didn't exist.

use std::collections::HashMap;

use axum::extract::{Path, Query, Request, State};
use axum::http::{Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use chrono::{DateTime, FixedOffset, TimeDelta, Utc};
use imbo_proto::ErrorCode;
use imbo_proto::api::{
    AccountRef, AdminAccount, AdminAccountResponse, AdminAccountsPage, AdminSubscription,
    BanResponse, DeleteAccountRequest, GroupRef, ReportFile, ReportMessage, ReportParty,
    ReportResponse, ReportView, ReportsPage, ResolveRequest, ResolveResponse, StatsDay,
    StatsResponse,
};
use imbo_proto::frames::CLOSE_UNAUTHORIZED;
use serde::Deserialize;
use serde_json::Value;
use tracing::info;
use uuid::Uuid;

use super::Body;
use crate::auth::{Caller, require_web_origin};
use crate::error::{ApiError, ApiResult, err};
use crate::state::St;
use crate::{avatar, billing, delivery, safety};

const PAGE: i64 = 50;

fn not_found() -> ApiError {
    err(ErrorCode::NotFound, "No such endpoint.")
}

/// Is this account an admin right now: listed email, not banned.
pub async fn is_admin(st: &St, account: i32) -> Result<bool, sqlx::Error> {
    if st.cfg.admin_emails.is_empty() {
        return Ok(false);
    }
    let row: Option<(String, bool)> = sqlx::query_as(
        "SELECT email, banned_at IS NOT NULL FROM accounts a
         WHERE id = $1 AND NOT EXISTS (SELECT 1 FROM deleted_accounts d WHERE d.id = a.id)",
    )
    .bind(account)
    .fetch_optional(&st.db)
    .await?;
    Ok(row.is_some_and(|(email, banned)| !banned && st.cfg.is_admin_email(&email)))
}

/// Middleware for `/v1/admin/*`: 401 when not signed in, 404 for anyone
/// but an admin on the web, and the web app's `Origin` on writes.
pub async fn gate(State(st): State<St>, mut req: Request, next: Next) -> Response {
    let caller = match crate::auth::resolve(&st, req.headers()).await {
        Ok(c) => c,
        Err(e) if e.0.is(ErrorCode::Unauthorized) || e.0.is(ErrorCode::RateLimited) => {
            return e.into_response();
        }
        Err(_) => return not_found().into_response(),
    };
    match caller.web {
        Some(_) => match is_admin(&st, caller.account_id).await {
            Ok(true) => {}
            Ok(false) => return not_found().into_response(),
            Err(e) => return ApiError::from(e).into_response(),
        },
        None => return not_found().into_response(),
    }
    if req.method() != Method::GET
        && let Err(e) = require_web_origin(&st, req.headers())
    {
        return e.into_response();
    }
    req.extensions_mut().insert(caller);
    next.run(req).await
}

fn page_of(raw: Option<i64>) -> ApiResult<i64> {
    match raw.unwrap_or(1) {
        p if p >= 1 => Ok(p),
        _ => Err(err(ErrorCode::InvalidRequest, "page starts at 1.")),
    }
}

fn path_id<T: std::str::FromStr>(raw: &str) -> ApiResult<T> {
    raw.parse().map_err(|_| not_found())
}

// ---- reports ----

#[derive(Deserialize)]
pub struct ReportsQuery {
    status: Option<String>,
    page: Option<i64>,
}

#[derive(sqlx::FromRow)]
struct ReportRow {
    id: i64,
    reporter_id: i32,
    target_id: i32,
    group_id: Option<i32>,
    group_name: Option<String>,
    reason: String,
    note: Option<String>,
    snapshot: Value,
    status: String,
    action: Option<String>,
    created_at: DateTime<Utc>,
    resolved_at: Option<DateTime<Utc>>,
}

const REPORT_COLUMNS: &str = "r.id, r.reporter_id, r.target_id, r.group_id, g.name AS group_name,
    r.reason, r.note, r.snapshot, r.status, r.action, r.created_at, r.resolved_at
    FROM reports r LEFT JOIN chat_groups g ON g.id = r.group_id";

/// Name, avatar and ban of each account a report shows.
async fn parties(st: &St, ids: &[i32]) -> ApiResult<HashMap<i32, ReportParty>> {
    let rows: Vec<(i32, String, bool)> =
        sqlx::query_as("SELECT id, name, banned_at IS NOT NULL FROM accounts WHERE id = ANY($1)")
            .bind(ids)
            .fetch_all(&st.db)
            .await?;
    let mut avatars = safety::avatars(&st.db, &st.cfg, ids).await?;
    Ok(rows
        .into_iter()
        .map(|(id, name, banned)| {
            let p = ReportParty {
                id,
                name,
                avatar_url: avatars.remove(&id),
                banned,
                email: None,
                avatar_changed: None,
            };
            (id, p)
        })
        .collect())
}

fn summary(r: &ReportRow, parties: &HashMap<i32, ReportParty>) -> ReportView {
    let party = |id: i32| {
        parties.get(&id).cloned().unwrap_or(ReportParty {
            id,
            name: String::new(),
            avatar_url: None,
            banned: false,
            email: None,
            avatar_changed: None,
        })
    };
    ReportView {
        id: r.id,
        reporter: party(r.reporter_id),
        target: party(r.target_id),
        group: match (r.group_id, &r.group_name) {
            (Some(id), Some(name)) => Some(GroupRef {
                id,
                name: name.clone(),
            }),
            _ => None,
        },
        reason: r.reason.clone(),
        status: r.status.clone(),
        created_at: r.created_at,
        note: None,
        messages: None,
        action: None,
        resolved_at: None,
    }
}

/// API-062: newest first, 50 a page; only open ones unless `status=all`.
pub async fn list_reports(
    State(st): State<St>,
    Query(q): Query<ReportsQuery>,
) -> ApiResult<Json<ReportsPage>> {
    let all = match q.status.as_deref() {
        None | Some("open") => false,
        Some("all") => true,
        Some(_) => {
            return Err(err(
                ErrorCode::InvalidRequest,
                "status must be open or all.",
            ));
        }
    };
    let page = page_of(q.page)?;
    let rows: Vec<ReportRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {REPORT_COLUMNS} WHERE ($1 OR r.status = 'open')
         ORDER BY r.created_at DESC, r.id DESC LIMIT $2 OFFSET $3"
    )))
    .bind(all)
    .bind(PAGE)
    .bind((page - 1) * PAGE)
    .fetch_all(&st.db)
    .await?;
    let total: i64 =
        sqlx::query_scalar("SELECT count(*) FROM reports WHERE ($1 OR status = 'open')")
            .bind(all)
            .fetch_one(&st.db)
            .await?;
    let mut ids: Vec<i32> = rows
        .iter()
        .flat_map(|r| [r.reporter_id, r.target_id])
        .collect();
    ids.sort_unstable();
    ids.dedup();
    let parties = parties(&st, &ids).await?;
    let reports = rows.iter().map(|r| summary(r, &parties)).collect();
    Ok(Json(ReportsPage { reports, total }))
}

/// One report with what was copied into it (API-063). Only these messages
/// are ever shown to an admin (INV-030).
async fn report_view(st: &St, id: i64) -> ApiResult<ReportView> {
    let r: ReportRow = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {REPORT_COLUMNS} WHERE r.id = $1"
    )))
    .bind(id)
    .fetch_optional(&st.db)
    .await?
    .ok_or_else(|| err(ErrorCode::NotFound, format!("No report {id}.")))?;
    let copied: Vec<Value> = r.snapshot.as_array().cloned().unwrap_or_default();
    let mut ids: Vec<i32> = copied
        .iter()
        .filter_map(|m| m["from"].as_i64())
        .map(|v| v as i32)
        .chain([r.reporter_id, r.target_id])
        .collect();
    ids.sort_unstable();
    ids.dedup();
    let parties = parties(st, &ids).await?;
    let mut view = summary(&r, &parties);
    let (email, avatar_at): (String, Option<DateTime<Utc>>) = sqlx::query_as(
        "SELECT a.email, av.created_at FROM accounts a LEFT JOIN avatars av ON av.id = a.avatar_id
         WHERE a.id = $1",
    )
    .bind(r.target_id)
    .fetch_one(&st.db)
    .await?;
    view.target.email = Some(email);
    view.target.avatar_changed = Some(avatar_at.is_some_and(|t| t > r.created_at));
    let messages = copied
        .iter()
        .map(|m| {
            let from = m["from"].as_i64().unwrap_or_default() as i32;
            ReportMessage {
                kind: m["kind"].as_str().unwrap_or("message").to_string(),
                from: AccountRef {
                    id: from,
                    name: parties
                        .get(&from)
                        .map(|p| p.name.clone())
                        .unwrap_or_default(),
                },
                body: m["body"].as_str().unwrap_or_default().to_string(),
                files: serde_json::from_value::<Vec<ReportFile>>(m["files"].clone())
                    .unwrap_or_default(),
                created_at: serde_json::from_value(m["created_at"].clone()).unwrap_or(r.created_at),
            }
        })
        .collect();
    view.note = r.note;
    view.messages = Some(messages);
    view.action = r.action;
    view.resolved_at = r.resolved_at;
    Ok(view)
}

/// API-063.
pub async fn get_report(
    State(st): State<St>,
    Path(raw): Path<String>,
) -> ApiResult<Json<ReportResponse>> {
    let id: i64 = path_id(&raw)?;
    Ok(Json(ReportResponse {
        report: report_view(&st, id).await?,
    }))
}

/// API-064: ban, remove the avatar, or dismiss; nobody is told.
pub async fn resolve_report(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
    Path(raw): Path<String>,
    Body(req): Body<ResolveRequest>,
) -> ApiResult<Json<ResolveResponse>> {
    let id: i64 = path_id(&raw)?;
    let action = req.action.as_str();
    if !matches!(action, "ban" | "remove_avatar" | "dismiss") {
        return Err(err(
            ErrorCode::InvalidRequest,
            "action must be ban, remove_avatar or dismiss.",
        ));
    }
    let mut tx = st.db.begin().await?;
    let target: Option<i32> = sqlx::query_scalar("SELECT target_id FROM reports WHERE id = $1")
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
    let Some(target) = target else {
        return Err(err(ErrorCode::NotFound, format!("No report {id}.")));
    };
    // The account row before the report row, the order a ban takes them in
    // (it closes the account's open reports), so the two can't deadlock.
    if action != "dismiss" {
        sqlx::query("SELECT 1 FROM accounts WHERE id = $1 FOR NO KEY UPDATE")
            .bind(target)
            .execute(&mut *tx)
            .await?;
    }
    let status: String = sqlx::query_scalar("SELECT status FROM reports WHERE id = $1 FOR UPDATE")
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
    if status != "open" {
        return Err(err(
            ErrorCode::ReportClosed,
            format!("Report {id} was already handled."),
        ));
    }
    let mut banned = None;
    match action {
        "ban" => {
            refuse_admin(&st, target).await?;
            banned = Some(safety::ban_tx(&mut tx, target, Some(c.account_id)).await?);
            close_report(&mut tx, id, "actioned", Some("ban"), c.account_id).await?;
        }
        "remove_avatar" => {
            let old: Option<Uuid> = sqlx::query_scalar(
                "SELECT avatar_id FROM accounts WHERE id = $1 FOR NO KEY UPDATE",
            )
            .bind(target)
            .fetch_one(&mut *tx)
            .await?;
            avatar::delete(&mut tx, &old.into_iter().collect::<Vec<_>>()).await?;
            close_report(&mut tx, id, "actioned", Some("remove_avatar"), c.account_id).await?;
        }
        _ => close_report(&mut tx, id, "dismissed", None, c.account_id).await?,
    }
    tx.commit().await?;
    info!(
        event = "report_resolved",
        report_id = id,
        action,
        "report resolved"
    );
    let subscription_canceled = match banned {
        Some(b) => Some(finish_ban(&st, target, b).await),
        None => None,
    };
    Ok(Json(ResolveResponse {
        report: report_view(&st, id).await?,
        subscription_canceled,
    }))
}

async fn close_report(
    tx: &mut sqlx::PgConnection,
    id: i64,
    status: &str,
    action: Option<&str>,
    by: i32,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE reports SET status = $2, action = $3, resolved_by = $4, resolved_at = now()
         WHERE id = $1 AND status = 'open'",
    )
    .bind(id)
    .bind(status)
    .bind(action)
    .bind(by)
    .execute(&mut *tx)
    .await?;
    Ok(())
}

/// An admin can't ban an admin (API-066).
async fn refuse_admin(st: &St, target: i32) -> ApiResult<()> {
    let email: Option<String> = sqlx::query_scalar("SELECT email FROM accounts WHERE id = $1")
        .bind(target)
        .fetch_optional(&st.db)
        .await?;
    if email.is_some_and(|e| st.cfg.is_admin_email(&e)) {
        return Err(err(ErrorCode::Forbidden, "Admins can't ban an admin."));
    }
    Ok(())
}

/// After the ban commits: close its connections, push the take-backs,
/// cancel the subscriptions.
async fn finish_ban(st: &St, target: i32, b: safety::Banned) -> bool {
    for d in &b.devices {
        st.hub.close(*d, CLOSE_UNAUTHORIZED, "account banned");
    }
    delivery::notify_canceled(st, b.canceled).await;
    safety::after_ban(&st.db, &st.cfg, &st.http, target, b.newly).await
}

// ---- accounts ----

#[derive(Deserialize)]
pub struct AccountsQuery {
    q: Option<String>,
    page: Option<i64>,
}

#[derive(sqlx::FromRow)]
struct AccountRow {
    id: i32,
    name: String,
    email: String,
    avatar_id: Option<Uuid>,
    created_at: DateTime<Utc>,
    banned_at: Option<DateTime<Utc>>,
    last_active_at: Option<DateTime<Utc>>,
}

const ACCOUNT_COLUMNS: &str = "a.id, a.name, a.email, a.avatar_id, a.created_at, a.banned_at,
    GREATEST((SELECT max(last_seen_at) FROM devices WHERE account_id = a.id),
             (SELECT max(created_at) FROM messages WHERE from_account = a.id)) AS last_active_at
    FROM accounts a WHERE NOT EXISTS (SELECT 1 FROM deleted_accounts d WHERE d.id = a.id)";

async fn admin_accounts(st: &St, rows: Vec<AccountRow>) -> ApiResult<Vec<AdminAccount>> {
    let ids: Vec<i32> = rows.iter().map(|r| r.id).collect();
    let access = billing::access_many(&st.db, &st.cfg, &ids).await?;
    // Newest first, so the first one per account is its latest.
    let subs = billing::subscriptions(&st.db, &st.cfg, &ids).await?;
    Ok(rows
        .into_iter()
        .map(|r| AdminAccount {
            access: access
                .get(&r.id)
                .cloned()
                .unwrap_or(billing::Access::Free)
                .view(),
            subscription: subs
                .iter()
                .find(|s| s.account_id == r.id)
                .map(|s| AdminSubscription {
                    status: s.status.clone(),
                    paid_until: s.paid_until,
                }),
            avatar_url: if r.banned_at.is_some() {
                None
            } else {
                avatar::url_of(&st.cfg, r.avatar_id)
            },
            admin: st.cfg.is_admin_email(&r.email),
            id: r.id,
            name: r.name,
            email: r.email,
            created_at: r.created_at,
            last_active_at: r.last_active_at,
            banned_at: r.banned_at,
        })
        .collect())
}

async fn admin_account(st: &St, id: i32) -> ApiResult<AdminAccount> {
    let row: Option<AccountRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {ACCOUNT_COLUMNS} AND a.id = $1"
    )))
    .bind(id)
    .fetch_optional(&st.db)
    .await?;
    let row = row.ok_or_else(|| {
        err(
            ErrorCode::AccountNotFound,
            format!("No account with ID {id}."),
        )
    })?;
    Ok(admin_accounts(st, vec![row]).await?.remove(0))
}

/// API-065: by exact account ID or part of the email, newest sign-up first.
pub async fn list_accounts(
    State(st): State<St>,
    Query(q): Query<AccountsQuery>,
) -> ApiResult<Json<AdminAccountsPage>> {
    let page = page_of(q.page)?;
    let text =
        q.q.map(|s| s.trim().to_lowercase())
            .filter(|s| !s.is_empty());
    let id: Option<i32> = text.as_deref().and_then(|t| t.parse().ok());
    let filter = "($1::text IS NULL OR a.id = $2 OR strpos(lower(a.email), $1) > 0)";
    let rows: Vec<AccountRow> = sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "SELECT {ACCOUNT_COLUMNS} AND {filter} ORDER BY a.created_at DESC, a.id DESC LIMIT $3 OFFSET $4"
    )))
    .bind(&text)
    .bind(id)
    .bind(PAGE)
    .bind((page - 1) * PAGE)
    .fetch_all(&st.db)
    .await?;
    let total: i64 = sqlx::query_scalar(sqlx::AssertSqlSafe(format!(
        "SELECT count(*) FROM accounts a
         WHERE NOT EXISTS (SELECT 1 FROM deleted_accounts d WHERE d.id = a.id) AND {filter}"
    )))
    .bind(&text)
    .bind(id)
    .fetch_one(&st.db)
    .await?;
    Ok(Json(AdminAccountsPage {
        accounts: admin_accounts(&st, rows).await?,
        total,
    }))
}

/// API-066: ban now; a second call only retries canceling the subscriptions.
pub async fn ban(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
    Path(raw): Path<String>,
) -> ApiResult<Json<BanResponse>> {
    let target: i32 = path_id(&raw)?;
    admin_account(&st, target).await?;
    refuse_admin(&st, target).await?;
    let mut tx = st.db.begin().await?;
    let b = safety::ban_tx(&mut tx, target, Some(c.account_id)).await?;
    tx.commit().await?;
    let subscription_canceled = finish_ban(&st, target, b).await;
    Ok(Json(BanResponse {
        account: admin_account(&st, target).await?,
        subscription_canceled,
    }))
}

/// API-067: 200 whether or not it was banned.
pub async fn unban(
    State(st): State<St>,
    Path(raw): Path<String>,
) -> ApiResult<Json<AdminAccountResponse>> {
    let target: i32 = path_id(&raw)?;
    safety::unban(&st.db, target).await?;
    Ok(Json(AdminAccountResponse {
        account: admin_account(&st, target).await?,
    }))
}

/// API-070: delete a banned account as API-052 would; reports stay.
pub async fn delete_account(
    State(st): State<St>,
    Path(raw): Path<String>,
    Body(req): Body<DeleteAccountRequest>,
) -> ApiResult<StatusCode> {
    let target: i32 = path_id(&raw)?;
    if req.confirm != "DELETE" {
        return Err(err(ErrorCode::InvalidRequest, "Type DELETE to confirm."));
    }
    let account = admin_account(&st, target).await?;
    if account.banned_at.is_none() {
        return Err(err(
            ErrorCode::InvalidRequest,
            "Only a banned account can be deleted here. Ban it first.",
        ));
    }
    super::app::delete_now(&st, target, "its owner's account was deleted").await?;
    info!(
        event = "banned_account_deleted",
        account_id = target,
        "deleted"
    );
    Ok(StatusCode::NO_CONTENT)
}

/// API-069: a personal or group avatar; nobody is told.
pub async fn remove_avatar(State(st): State<St>, Path(raw): Path<String>) -> ApiResult<StatusCode> {
    let id: Uuid = Uuid::parse_str(raw.trim_end_matches(".jpg")).map_err(|_| no_avatar())?;
    let r = sqlx::query("DELETE FROM avatars WHERE id = $1")
        .bind(id)
        .execute(&st.db)
        .await?;
    if r.rows_affected() == 0 {
        return Err(no_avatar());
    }
    info!(event = "avatar_removed", %id, "avatar removed by admin");
    Ok(StatusCode::NO_CONTENT)
}

fn no_avatar() -> ApiError {
    err(ErrorCode::NotFound, "No such avatar.")
}

// ---- stats ----

/// Start of today in Beijing time, as a UTC instant.
fn beijing_today(now: DateTime<Utc>) -> DateTime<Utc> {
    let bj = FixedOffset::east_opt(8 * 3600).expect("valid offset");
    now.with_timezone(&bj)
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .expect("midnight")
        .and_local_timezone(bj)
        .single()
        .expect("fixed offset")
        .with_timezone(&Utc)
}

/// API-068: counted afresh on every call.
pub async fn stats(State(st): State<St>) -> ApiResult<Json<StatsResponse>> {
    let now = Utc::now();
    let today = beijing_today(now);
    let (accounts, active_7d, messages_today): (i64, i64, i64) = sqlx::query_as(
        "SELECT
           (SELECT count(*) FROM accounts a WHERE NOT EXISTS (SELECT 1 FROM deleted_accounts d WHERE d.id = a.id)),
           (SELECT count(DISTINCT from_account) FROM messages WHERE created_at > now() - interval '7 days'),
           (SELECT count(*) FROM messages WHERE created_at >= $1)",
    )
    .bind(today)
    .fetch_one(&st.db)
    .await?;
    // One row per day, the oldest first; `end` is the next midnight.
    let rows: Vec<(i32, i64, i64, i64)> = sqlx::query_as(
        "SELECT i,
           (SELECT count(*) FROM accounts a WHERE a.created_at < e.end_at
              AND NOT EXISTS (SELECT 1 FROM deleted_accounts d WHERE d.id = a.id)),
           (SELECT count(DISTINCT from_account) FROM messages
              WHERE created_at >= e.end_at - interval '7 days' AND created_at < e.end_at),
           (SELECT count(*) FROM messages
              WHERE created_at >= e.end_at - interval '1 day' AND created_at < e.end_at)
         FROM (SELECT i, $1::timestamptz - make_interval(days => i - 1) AS end_at
               FROM generate_series(0, 29) AS i) e
         ORDER BY i DESC",
    )
    .bind(today)
    .fetch_all(&st.db)
    .await?;
    let bj = FixedOffset::east_opt(8 * 3600).expect("valid offset");
    let days = rows
        .into_iter()
        .map(|(i, accounts, active_7d, messages)| StatsDay {
            date: (today - TimeDelta::days(i as i64))
                .with_timezone(&bj)
                .format("%Y-%m-%d")
                .to_string(),
            accounts,
            active_7d,
            messages,
        })
        .collect();
    let subs: Vec<billing::SubRow> = match &st.cfg.waffo {
        Some(waffo) => {
            sqlx::query_as(
                "SELECT account_id, plan, status, paid_until, revoked_at FROM subscriptions
                 WHERE mode = $1",
            )
            .bind(waffo.mode())
            .fetch_all(&st.db)
            .await?
        }
        None => vec![],
    };
    let paid = subs
        .iter()
        .filter(|s| s.grants_until().is_some_and(|t| t > now))
        .count() as i64;
    Ok(Json(StatsResponse {
        accounts,
        active_7d,
        paid,
        messages_today,
        days,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn beijing_day_starts_at_16_utc() {
        let t: DateTime<Utc> = "2026-10-27T15:59:00Z".parse().unwrap();
        assert_eq!(
            beijing_today(t),
            "2026-10-26T16:00:00Z".parse::<DateTime<Utc>>().unwrap()
        );
        let t: DateTime<Utc> = "2026-10-27T16:00:00Z".parse().unwrap();
        assert_eq!(
            beijing_today(t),
            "2026-10-27T16:00:00Z".parse::<DateTime<Utc>>().unwrap()
        );
    }
}
