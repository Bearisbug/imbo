//! API-008..013: contact requests, accept/decline (contacts, group invites
//! and requests to join a group), contact list and removal.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::{Extension, Json};
use chrono::{DateTime, Utc};
use imbo_proto::api::{
    AcceptBody, AcceptResponse, AccountRef, ContactItem, ContactRequestBody, ContactsResponse,
    GroupRef, RequestItem, RequestResponse, RequestsResponse, SHARE_ALL, SHARE_CHOSEN, SHARE_NONE,
    SessionInfo, ShareBody, ShareDevice, ShareResponse, ShareSession,
};
use imbo_proto::text::valid_email;
use imbo_proto::{ErrorCode, MAX_NOTE_CHARS};
use serde::Deserialize;
use sqlx::{Postgres, Transaction};
use tracing::info;
use uuid::Uuid;

use super::Body;
use crate::auth::Caller;
use crate::error::{ApiResult, err, rate_limited};
use crate::state::St;
use crate::{delivery, notify, safety, views};

#[derive(sqlx::FromRow)]
struct RequestRow {
    id: i64,
    kind: String,
    from_account: i32,
    from_name: String,
    to_account: Option<i32>,
    to_name: Option<String>,
    group_id: Option<i32>,
    group_name: Option<String>,
    note: Option<String>,
    status: String,
    decided_by: Option<String>,
    created_at: DateTime<Utc>,
}

const REQUEST_SELECT: &str = "SELECT r.id, r.kind, r.from_account, fa.name AS from_name, r.to_account,
        ta.name AS to_name, r.group_id, g.name AS group_name, r.note, r.status, r.decided_by, r.created_at
     FROM requests r
     JOIN accounts fa ON fa.id = r.from_account
     LEFT JOIN accounts ta ON ta.id = r.to_account
     LEFT JOIN chat_groups g ON g.id = r.group_id";

impl RequestRow {
    /// `show_note`: only the receiving person sees the note (RULE-002).
    fn into_item(self, show_note: bool) -> RequestItem {
        RequestItem {
            id: self.id,
            kind: self.kind,
            from: AccountRef {
                id: self.from_account,
                name: self.from_name,
            },
            to: match (self.to_account, self.to_name) {
                (Some(id), Some(name)) => Some(AccountRef { id, name }),
                _ => None,
            },
            group: match (self.group_id, self.group_name) {
                (Some(id), Some(name)) => Some(GroupRef { id, name }),
                _ => None,
            },
            note: if show_note { self.note } else { None },
            status: self.status,
            decided_by: self.decided_by,
            created_at: self.created_at,
        }
    }
}

async fn load_request(st: &St, id: i64) -> Result<Option<RequestRow>, sqlx::Error> {
    sqlx::query_as(sqlx::AssertSqlSafe(format!(
        "{REQUEST_SELECT} WHERE r.id = $1"
    )))
    .bind(id)
    .fetch_optional(&st.db)
    .await
}

async fn lock_pair(tx: &mut Transaction<'_, Postgres>, a: i32, b: i32) -> Result<(), sqlx::Error> {
    let (lo, hi) = if a < b { (a, b) } else { (b, a) };
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(((lo as i64) << 32) | hi as i64)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

async fn add_contact_rows(
    tx: &mut Transaction<'_, Postgres>,
    a: i32,
    b: i32,
) -> Result<(), sqlx::Error> {
    sqlx::query("INSERT INTO contacts (account_id, peer_id) VALUES ($1, $2), ($2, $1) ON CONFLICT DO NOTHING")
        .bind(a)
        .bind(b)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

fn agent_label(c: &Caller) -> String {
    match &c.actor {
        Some(a) => format!("Your agent \"{}\" ({})", a.title, a.short_id),
        None => "You".into(),
    }
}

/// API-008 by email: the account it belongs to, told as plainly as an
/// account ID would be (ADR-051).
async fn account_by_email(st: &St, c: &Caller, raw: &str) -> ApiResult<i32> {
    let email = raw.trim().to_lowercase();
    if !valid_email(&email) {
        return Err(err(
            ErrorCode::InvalidEmail,
            "That email address doesn't look right.",
        ));
    }
    let id: Option<i32> = sqlx::query_scalar("SELECT id FROM accounts WHERE lower(email) = $1")
        .bind(&email)
        .fetch_optional(&st.db)
        .await?;
    match id {
        Some(id) if id == c.account_id => {
            Err(err(ErrorCode::CannotAddSelf, "That is your own email."))
        }
        Some(id) => Ok(id),
        None => Err(err(
            ErrorCode::AccountNotFound,
            format!("No account with email {email}."),
        )),
    }
}

pub async fn request_contact(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
    Body(req): Body<ContactRequestBody>,
) -> ApiResult<Response> {
    crate::billing::require_active(&st, c.account_id, "API-008").await?;
    let target = match (req.account_id, req.email.as_deref()) {
        (Some(id), None) => {
            if id == c.account_id {
                return Err(err(
                    ErrorCode::CannotAddSelf,
                    "That is your own account ID.",
                ));
            }
            id
        }
        (None, Some(email)) => account_by_email(&st, &c, email).await?,
        _ => {
            return Err(err(
                ErrorCode::InvalidRequest,
                "Give either an account ID or an email.",
            ));
        }
    };
    let note = req
        .note
        .map(|n| n.trim().to_string())
        .filter(|n| !n.is_empty());
    if note
        .as_ref()
        .is_some_and(|n| n.chars().count() > MAX_NOTE_CHARS)
    {
        return Err(err(
            ErrorCode::InvalidRequest,
            "The note can be at most 200 characters.",
        ));
    }
    let Some(peer) = views::account_ref(&st.db, target).await? else {
        return Err(err(
            ErrorCode::AccountNotFound,
            format!("No account with ID {target}."),
        ));
    };

    let mut tx = st.db.begin().await?;
    lock_pair(&mut tx, c.account_id, target).await?;
    // A block of me or a ban of either of us in progress finishes first:
    // both lock the rows shared here (DESIGN §16).
    let banned: bool =
        sqlx::query_scalar("SELECT banned_at IS NOT NULL FROM accounts WHERE id = $1 FOR SHARE")
            .bind(c.account_id)
            .fetch_one(&mut *tx)
            .await?;
    if banned {
        return Err(err(ErrorCode::Unauthorized, "This account was banned."));
    }
    safety::check_blocks(&mut tx, c.account_id, target, &peer.name).await?;
    safety::check_not_banned(&mut tx, target, &peer.name).await?;
    let already: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM contacts WHERE account_id = $1 AND peer_id = $2)",
    )
    .bind(c.account_id)
    .bind(target)
    .fetch_one(&mut *tx)
    .await?;
    if already {
        return Err(err(
            ErrorCode::AlreadyContacts,
            format!("{} is already your contact.", peer.name),
        ));
    }
    // The other side already asked: accept theirs (ENT-Request pending → accepted).
    let reverse: Option<i64> = sqlx::query_scalar(
        "UPDATE requests SET status = 'accepted', decided_by = $3, decided_at = now()
         WHERE kind = 'contact' AND from_account = $1 AND to_account = $2 AND status = 'pending' RETURNING id",
    )
    .bind(target)
    .bind(c.account_id)
    .bind(c.decided_by())
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(req_id) = reverse {
        add_contact_rows(&mut tx, c.account_id, target).await?;
        tx.commit().await?;
        notify::send(
            &st,
            target,
            "request_accepted",
            format!(
                "{} ({}) accepted your contact request.",
                c.name, c.account_id
            ),
            None,
            Some(req_id),
        );
        if c.is_agent() {
            notify::send(
                &st,
                c.account_id,
                "agent_accepted",
                format!(
                    "{} accepted the contact request from {} ({}). Undo with `imbo contact remove {}`.",
                    agent_label(&c),
                    peer.name,
                    peer.id,
                    peer.id
                ),
                None,
                Some(req_id),
            );
        }
        return Ok((StatusCode::OK, Json(serde_json::json!({ "contact": peer }))).into_response());
    }
    let pending: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM requests WHERE kind = 'contact' AND from_account = $1 AND to_account = $2 AND status = 'pending')",
    )
    .bind(c.account_id)
    .bind(target)
    .fetch_one(&mut *tx)
    .await?;
    if pending {
        return Err(err(
            ErrorCode::RequestPending,
            format!("You already have a pending request to {}.", peer.name),
        ));
    }
    let today: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM requests WHERE from_account = $1 AND kind = 'contact' AND created_at > now() - interval '1 day'",
    )
    .bind(c.account_id)
    .fetch_one(&mut *tx)
    .await?;
    if today >= 20 {
        return Err(rate_limited(
            "contact-requests",
            Some(c.account_id),
            "At most 20 contact requests per day.",
            3600,
        ));
    }
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO requests (kind, from_account, to_account, note) VALUES ('contact', $1, $2, $3) RETURNING id",
    )
    .bind(c.account_id)
    .bind(target)
    .bind(&note)
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
    notify::send(
        &st,
        target,
        "contact_request",
        format!(
            "{} ({}) wants to add you as a contact. `imbo contact requests` to review.",
            c.name, c.account_id
        ),
        note,
        Some(id),
    );
    let item = load_request(&st, id)
        .await?
        .expect("just inserted")
        .into_item(false);
    Ok((StatusCode::CREATED, Json(RequestResponse { request: item })).into_response())
}

#[derive(Deserialize)]
pub struct RequestsQuery {
    direction: Option<String>,
    status: Option<String>,
    include: Option<String>,
}

/// The owner and admins of a group, in person, see and handle its join
/// requests (RULE-024).
const HANDLES_JOIN: &str = "EXISTS (SELECT 1 FROM group_members m WHERE m.group_id = r.group_id
        AND m.account_id = $1 AND m.role IN ('owner', 'admin'))";

pub async fn list_requests(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
    Query(q): Query<RequestsQuery>,
) -> ApiResult<Json<RequestsResponse>> {
    let outgoing = q.direction.as_deref() == Some("out");
    let all = q.status.as_deref() == Some("all");
    // Only with include=join, so older clients, which need `to`, never get one.
    let join = q.include.as_deref() == Some("join") && !c.is_agent();
    let whose = match (outgoing, join) {
        (true, true) => "r.from_account = $1".to_string(),
        (true, false) => "r.from_account = $1 AND r.kind <> 'join'".to_string(),
        (false, true) => format!("(r.to_account = $1 OR (r.kind = 'join' AND {HANDLES_JOIN}))"),
        (false, false) => "r.to_account = $1".to_string(),
    };
    let sql = format!(
        "{REQUEST_SELECT} WHERE {whose} {} ORDER BY r.id DESC LIMIT 100",
        if all { "" } else { "AND r.status = 'pending'" },
    );
    let rows: Vec<RequestRow> = sqlx::query_as(sqlx::AssertSqlSafe(sql))
        .bind(c.account_id)
        .fetch_all(&st.db)
        .await?;
    let show_note = !outgoing && !c.is_agent();
    let notes_hidden = !show_note && rows.iter().any(|r| r.note.is_some());
    Ok(Json(RequestsResponse {
        requests: rows.into_iter().map(|r| r.into_item(show_note)).collect(),
        notes_hidden,
    }))
}

async fn my_session_choice(st: &St, c: &Caller, asked: Option<Vec<Uuid>>) -> ApiResult<Vec<Uuid>> {
    let wanted = match asked {
        Some(v) => v,
        None => c.actor.as_ref().map(|a| vec![a.id]).unwrap_or_default(),
    };
    if wanted.is_empty() {
        return Ok(wanted);
    }
    let owned: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM agents WHERE account_id = $1 AND id = ANY($2) AND tool <> 'terminal'",
    )
    .bind(c.account_id)
    .bind(&wanted)
    .fetch_all(&st.db)
    .await?;
    if owned.len() != {
        let mut w = wanted.clone();
        w.sort();
        w.dedup();
        w.len()
    } {
        return Err(err(
            ErrorCode::SessionNotFound,
            "Some of those sessions are not yours.",
        ));
    }
    Ok(owned)
}

pub async fn accept(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
    Path(id): Path<i64>,
    raw: axum::body::Bytes,
) -> ApiResult<Response> {
    crate::billing::require_active(&st, c.account_id, "API-010").await?;
    let body: AcceptBody = if raw.iter().all(|b| b.is_ascii_whitespace()) {
        AcceptBody::default()
    } else {
        serde_json::from_slice(&raw)
            .map_err(|e| err(ErrorCode::InvalidRequest, format!("Invalid JSON body: {e}")))?
    };
    let Some(row) = load_request(&st, id).await?.filter(|r| {
        r.to_account == Some(c.account_id) || (r.kind == "join" && !c.is_agent())
    }) else {
        return Err(err(ErrorCode::NotFound, "No such request."));
    };
    if row.kind == "join" {
        return accept_join(&st, &c, row).await;
    }
    if row.status != "pending" {
        return Err(err(
            ErrorCode::RequestClosed,
            format!("This request was already {}.", row.status),
        ));
    }
    let sessions = if row.kind == "group" {
        my_session_choice(&st, &c, body.sessions).await?
    } else {
        vec![]
    };

    let mut tx = st.db.begin().await?;
    let changed = sqlx::query(
        "UPDATE requests SET status = 'accepted', decided_by = $2, decided_at = now() WHERE id = $1 AND status = 'pending'",
    )
    .bind(id)
    .bind(c.decided_by())
    .execute(&mut *tx)
    .await?;
    if changed.rows_affected() == 0 {
        return Err(err(
            ErrorCode::RequestClosed,
            "This request was already handled.",
        ));
    }
    let from = AccountRef {
        id: row.from_account,
        name: row.from_name.clone(),
    };
    let resp = if row.kind == "contact" {
        lock_pair(&mut tx, c.account_id, row.from_account).await?;
        add_contact_rows(&mut tx, c.account_id, row.from_account).await?;
        tx.commit().await?;
        notify::send(
            &st,
            row.from_account,
            "request_accepted",
            format!(
                "{} ({}) accepted your contact request.",
                c.name, c.account_id
            ),
            None,
            Some(id),
        );
        if c.is_agent() {
            notify::send(
                &st,
                c.account_id,
                "agent_accepted",
                format!(
                    "{} accepted the contact request from {} ({}). Undo with `imbo contact remove {}`.",
                    agent_label(&c),
                    from.name,
                    from.id,
                    from.id
                ),
                None,
                Some(id),
            );
        }
        AcceptResponse {
            contact: Some(from),
            group: None,
            my_sessions: None,
        }
    } else {
        let gid = row.group_id.unwrap_or_default();
        let group = GroupRef {
            id: gid,
            name: row.group_name.clone().unwrap_or_default(),
        };
        // Under the group lock, like leaving, removing and dissolving (DESIGN §16).
        super::groups::lock_group(&mut tx, gid).await?;
        let (dissolved, removed, by_owner): (bool, bool, bool) = sqlx::query_as(
            "SELECT g.dissolved_at IS NOT NULL,
                    EXISTS (SELECT 1 FROM group_removals WHERE group_id = g.id AND account_id = $2),
                    EXISTS (SELECT 1 FROM group_members WHERE group_id = g.id AND account_id = $3 AND role = 'owner')
             FROM chat_groups g WHERE g.id = $1",
        )
        .bind(gid)
        .bind(c.account_id)
        .bind(row.from_account)
        .fetch_one(&mut *tx)
        .await?;
        if dissolved {
            return Err(err(
                ErrorCode::NotFound,
                format!("Group {} (#{gid}) was dissolved.", group.name),
            ));
        }
        // RULE-009: checked again when the invite is accepted.
        if removed && !by_owner {
            return Err(err(
                ErrorCode::RemovedFromGroup,
                format!(
                    "You were removed from {} by its owner. Only the owner can invite you back.",
                    group.name
                ),
            ));
        }
        let joined = sqlx::query("INSERT INTO group_members (group_id, account_id) VALUES ($1, $2) ON CONFLICT DO NOTHING")
            .bind(gid)
            .bind(c.account_id)
            .execute(&mut *tx)
            .await?;
        if joined.rows_affected() == 0 {
            return Err(err(
                ErrorCode::AlreadyMember,
                format!("You are already in {}.", group.name),
            ));
        }
        sqlx::query("DELETE FROM group_removals WHERE group_id = $1 AND account_id = $2")
            .bind(gid)
            .bind(c.account_id)
            .execute(&mut *tx)
            .await?;
        for agent in &sessions {
            sqlx::query("INSERT INTO member_sessions (group_id, account_id, agent_id) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING")
                .bind(gid)
                .bind(c.account_id)
                .bind(agent)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        if c.is_agent() {
            notify::send(
                &st,
                c.account_id,
                "agent_accepted",
                format!(
                    "{} joined group {} (#{}) on your behalf. Leave with `imbo group leave #{}`.",
                    agent_label(&c),
                    group.name,
                    gid,
                    gid
                ),
                None,
                Some(id),
            );
        }
        AcceptResponse {
            contact: None,
            group: Some(group),
            my_sessions: Some(views::agent_refs(&st.db, &sessions).await?),
        }
    };
    Ok(Json(resp).into_response())
}

/// Under the group lock: the account's role there, or None when it's not
/// the owner or an admin of a group that still exists (RULE-024).
async fn join_handler_role(
    tx: &mut Transaction<'_, Postgres>,
    gid: i32,
    account: i32,
) -> Result<Option<String>, sqlx::Error> {
    super::groups::lock_group(tx, gid).await?;
    sqlx::query_scalar(
        "SELECT m.role FROM group_members m JOIN chat_groups g ON g.id = m.group_id
         WHERE m.group_id = $1 AND m.account_id = $2 AND m.role IN ('owner', 'admin')
           AND g.dissolved_at IS NULL",
    )
    .bind(gid)
    .bind(account)
    .fetch_optional(&mut **tx)
    .await
}

/// API-010 on a request to join: the group's owner or an admin, in person,
/// lets the asker in with no sessions picked (FLOW-GROUP-APPROVE).
async fn accept_join(st: &St, c: &Caller, row: RequestRow) -> ApiResult<Response> {
    let gid = row.group_id.unwrap_or_default();
    let name = row.group_name.clone().unwrap_or_default();
    let mut tx = st.db.begin().await?;
    let Some(role) = join_handler_role(&mut tx, gid, c.account_id).await? else {
        return Err(err(ErrorCode::NotFound, "No such request."));
    };
    let changed = sqlx::query(
        "UPDATE requests SET status = 'accepted', decided_by = $2, decided_at = now() WHERE id = $1 AND status = 'pending'",
    )
    .bind(row.id)
    .bind(c.decided_by())
    .execute(&mut *tx)
    .await?;
    if changed.rows_affected() == 0 {
        return Err(err(
            ErrorCode::RequestClosed,
            "This request was already handled.",
        ));
    }
    // RULE-009: someone removed comes back only with the owner's yes.
    let removed: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM group_removals WHERE group_id = $1 AND account_id = $2)",
    )
    .bind(gid)
    .bind(row.from_account)
    .fetch_one(&mut *tx)
    .await?;
    if removed && role != "owner" {
        return Err(err(
            ErrorCode::RemovedFromGroup,
            format!(
                "{} was removed from {name}. Only its owner can let them back in.",
                row.from_name
            ),
        ));
    }
    let joined = sqlx::query("INSERT INTO group_members (group_id, account_id) VALUES ($1, $2) ON CONFLICT DO NOTHING")
        .bind(gid)
        .bind(row.from_account)
        .execute(&mut *tx)
        .await?;
    if joined.rows_affected() == 0 {
        return Err(err(
            ErrorCode::AlreadyMember,
            format!("{} is already in {name}.", row.from_name),
        ));
    }
    sqlx::query("DELETE FROM group_removals WHERE group_id = $1 AND account_id = $2")
        .bind(gid)
        .bind(row.from_account)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    notify::send(
        st,
        row.from_account,
        "join_accepted",
        format!(
            "{} ({}) let you into group {name} (#{gid}). Pick the sessions that get its messages: `imbo group sessions #{gid} --sessions <short IDs>`.",
            c.name, c.account_id
        ),
        None,
        Some(row.id),
    );
    let item = load_request(st, row.id)
        .await?
        .expect("exists")
        .into_item(true);
    Ok(Json(RequestResponse { request: item }).into_response())
}

pub async fn decline(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
    Path(id): Path<i64>,
) -> ApiResult<Json<RequestResponse>> {
    let Some(row) = load_request(&st, id).await?.filter(|r| {
        r.to_account == Some(c.account_id) || (r.kind == "join" && !c.is_agent())
    }) else {
        return Err(err(ErrorCode::NotFound, "No such request."));
    };
    let mut tx = st.db.begin().await?;
    if row.kind == "join"
        && join_handler_role(&mut tx, row.group_id.unwrap_or_default(), c.account_id)
            .await?
            .is_none()
    {
        return Err(err(ErrorCode::NotFound, "No such request."));
    }
    if row.status != "pending" {
        return Err(err(
            ErrorCode::RequestClosed,
            format!("This request was already {}.", row.status),
        ));
    }
    let changed = sqlx::query(
        "UPDATE requests SET status = 'declined', decided_by = $2, decided_at = now() WHERE id = $1 AND status = 'pending'",
    )
    .bind(id)
    .bind(c.decided_by())
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    if changed.rows_affected() == 0 {
        return Err(err(
            ErrorCode::RequestClosed,
            "This request was already handled.",
        ));
    }
    let item = load_request(&st, id)
        .await?
        .expect("exists")
        .into_item(!c.is_agent());
    Ok(Json(RequestResponse { request: item }))
}

#[derive(Deserialize)]
pub struct ContactsQuery {
    offline: Option<bool>,
}

#[derive(sqlx::FromRow)]
struct ContactSessionRow {
    peer_id: i32,
    peer_name: String,
    short_id: Option<String>,
    title: Option<String>,
    tool: Option<String>,
    online: Option<bool>,
    last_online_at: Option<DateTime<Utc>>,
}

pub async fn list_contacts(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
    Query(q): Query<ContactsQuery>,
) -> ApiResult<Json<ContactsResponse>> {
    let offline = q.offline.unwrap_or(false);
    let rows: Vec<ContactSessionRow> = sqlx::query_as(
        "SELECT a.id AS peer_id, a.name AS peer_name, ag.short_id, coalesce(ag.custom_title, ag.title) AS title, ag.tool, ag.online, ag.last_online_at
         FROM contacts ct
         JOIN accounts a ON a.id = ct.peer_id
         LEFT JOIN agents ag ON ag.account_id = a.id AND ag.tool <> 'terminal'
              AND (ag.online OR ($2 AND ag.last_online_at > now() - interval '7 days'))
              AND imbo_session_visible(a.id, $1, ag.id, ag.device_id)
         WHERE ct.account_id = $1
         ORDER BY a.name, a.id, ag.online DESC, coalesce(ag.custom_title, ag.title)",
    )
    .bind(c.account_id)
    .bind(offline)
    .fetch_all(&st.db)
    .await?;
    let mut contacts: Vec<ContactItem> = vec![];
    for r in rows {
        if contacts.last().map(|x| x.id) != Some(r.peer_id) {
            contacts.push(ContactItem {
                id: r.peer_id,
                name: r.peer_name.clone(),
                sessions: vec![],
                active: true,
                avatar_url: None,
                blocked: false,
            });
        }
        if let (Some(short_id), Some(title), Some(tool), Some(online)) =
            (r.short_id, r.title, r.tool, r.online)
        {
            contacts.last_mut().unwrap().sessions.push(SessionInfo {
                short_id,
                title,
                tool,
                online,
                last_online_at: r.last_online_at,
            });
        }
    }
    let ids: Vec<i32> = contacts.iter().map(|x| x.id).collect();
    let inactive = safety::inactive(&st.db, &st.cfg, &ids).await?;
    let mut avatars = safety::avatars(&st.db, &st.cfg, &ids).await?;
    let blocked = safety::blocked_by(&st.db, c.account_id, &ids).await?;
    for x in &mut contacts {
        x.active = !inactive.contains(&x.id);
        x.avatar_url = avatars.remove(&x.id);
        x.blocked = blocked.contains(&x.id);
    }
    Ok(Json(ContactsResponse { contacts }))
}

pub async fn remove_contact(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
    Path(peer): Path<i32>,
) -> ApiResult<StatusCode> {
    c.require_person()?;
    let mut tx = st.db.begin().await?;
    lock_pair(&mut tx, c.account_id, peer).await?;
    let removed = sqlx::query(
        "DELETE FROM contacts WHERE (account_id = $1 AND peer_id = $2) OR (account_id = $2 AND peer_id = $1)",
    )
    .bind(c.account_id)
    .bind(peer)
    .execute(&mut *tx)
    .await?;
    if removed.rows_affected() == 0 {
        return Err(err(
            ErrorCode::NotContact,
            "That account is not your contact.",
        ));
    }
    let (lo, hi) = if c.account_id < peer {
        (c.account_id, peer)
    } else {
        (peer, c.account_id)
    };
    let canceled: Vec<(i64, Uuid)> = sqlx::query_as(
        "UPDATE deliveries d SET state = 'canceled', canceled_at = now(), cancel_reason = 'contact_removed'
         FROM messages m JOIN conversations cv ON cv.id = m.conversation_id
         WHERE d.message_id = m.id AND cv.account_lo = $1 AND cv.account_hi = $2
           AND d.state IN ('pending', 'pushed')
         RETURNING d.message_id, d.agent_id",
    )
    .bind(lo)
    .bind(hi)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    delivery::notify_canceled(&st, canceled).await;
    notify::send(
        &st,
        peer,
        "contact_removed",
        format!(
            "{} ({}) removed you from their contacts.",
            c.name, c.account_id
        ),
        None,
        None,
    );
    Ok(StatusCode::NO_CONTENT)
}

/// Most devices and sessions one contact's list may hold (API-082).
const MAX_SHARED: usize = 200;

#[derive(sqlx::FromRow)]
struct SharedSessionRow {
    id: Uuid,
    short_id: String,
    title: String,
    tool: String,
    device_id: Option<Uuid>,
    online: bool,
}

/// What `peer` sees of my sessions (API-081); every chosen session, however
/// long it has been offline.
async fn share_view(db: &mut sqlx::PgConnection, me: i32, peer: i32) -> ApiResult<ShareResponse> {
    let share: Option<String> =
        sqlx::query_scalar("SELECT share FROM contacts WHERE account_id = $1 AND peer_id = $2")
            .bind(me)
            .bind(peer)
            .fetch_optional(&mut *db)
            .await?;
    let Some(share) = share else {
        return Err(err(
            ErrorCode::NotContact,
            "That account is not your contact.",
        ));
    };
    let devices: Vec<(Uuid, String)> = sqlx::query_as(
        "SELECT d.id, d.name FROM contact_shares cs JOIN devices d ON d.id = cs.device_id
         WHERE cs.account_id = $1 AND cs.peer_id = $2 ORDER BY d.name, d.id",
    )
    .bind(me)
    .bind(peer)
    .fetch_all(&mut *db)
    .await?;
    let sessions: Vec<SharedSessionRow> = sqlx::query_as(
        "SELECT ag.id, ag.short_id, coalesce(ag.custom_title, ag.title) AS title, ag.tool, ag.device_id, ag.online
         FROM contact_shares cs JOIN agents ag ON ag.id = cs.agent_id
         WHERE cs.account_id = $1 AND cs.peer_id = $2
         ORDER BY ag.online DESC, ag.last_online_at DESC NULLS LAST, ag.id",
    )
    .bind(me)
    .bind(peer)
    .fetch_all(&mut *db)
    .await?;
    Ok(ShareResponse {
        share,
        devices: devices
            .into_iter()
            .map(|(id, name)| ShareDevice { id, name })
            .collect(),
        sessions: sessions
            .into_iter()
            .map(|r| ShareSession {
                id: r.id,
                short_id: r.short_id,
                title: r.title,
                tool: r.tool,
                device_id: r.device_id,
                online: r.online,
            })
            .collect(),
    })
}

/// API-081.
pub async fn get_share(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
    Path(peer): Path<i32>,
) -> ApiResult<Json<ShareResponse>> {
    let mut conn = st.db.acquire().await?;
    Ok(Json(share_view(&mut conn, c.account_id, peer).await?))
}

/// API-082: replaces the whole setting for one contact.
pub async fn set_share(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
    Path(peer): Path<i32>,
    Body(req): Body<ShareBody>,
) -> ApiResult<Json<ShareResponse>> {
    c.require_person()?;
    let share = req.share.as_str();
    if ![SHARE_ALL, SHARE_CHOSEN, SHARE_NONE].contains(&share) {
        return Err(err(
            ErrorCode::InvalidRequest,
            "share must be all, chosen or none.",
        ));
    }
    let (mut devices, mut sessions) = if share == SHARE_CHOSEN {
        (
            req.devices.unwrap_or_default(),
            req.sessions.unwrap_or_default(),
        )
    } else {
        (vec![], vec![])
    };
    devices.sort_unstable();
    devices.dedup();
    sessions.sort_unstable();
    sessions.dedup();
    if devices.len() + sessions.len() > MAX_SHARED {
        return Err(err(
            ErrorCode::InvalidRequest,
            format!("Choose at most {MAX_SHARED} computers and sessions."),
        ));
    }
    let mut tx = st.db.begin().await?;
    let found: Option<i32> = sqlx::query_scalar(
        "SELECT peer_id FROM contacts WHERE account_id = $1 AND peer_id = $2 FOR UPDATE",
    )
    .bind(c.account_id)
    .bind(peer)
    .fetch_optional(&mut *tx)
    .await?;
    if found.is_none() {
        return Err(err(
            ErrorCode::NotContact,
            "That account is not your contact.",
        ));
    }
    let mine: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM devices WHERE account_id = $1 AND id = ANY($2) AND revoked_at IS NULL",
    )
    .bind(c.account_id)
    .bind(&devices)
    .fetch_one(&mut *tx)
    .await?;
    if mine as usize != devices.len() {
        return Err(err(
            ErrorCode::InvalidRequest,
            "Some of those computers aren't yours or are signed out.",
        ));
    }
    let mine: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM agents WHERE account_id = $1 AND id = ANY($2) AND tool <> 'terminal'",
    )
    .bind(c.account_id)
    .bind(&sessions)
    .fetch_one(&mut *tx)
    .await?;
    if mine as usize != sessions.len() {
        return Err(err(
            ErrorCode::InvalidRequest,
            "Some of those sessions aren't yours.",
        ));
    }
    sqlx::query("UPDATE contacts SET share = $3 WHERE account_id = $1 AND peer_id = $2")
        .bind(c.account_id)
        .bind(peer)
        .bind(share)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM contact_shares WHERE account_id = $1 AND peer_id = $2")
        .bind(c.account_id)
        .bind(peer)
        .execute(&mut *tx)
        .await?;
    let ids: Vec<Uuid> = (0..devices.len() + sessions.len())
        .map(|_| Uuid::now_v7())
        .collect();
    let (device_ids, agent_ids) = ids.split_at(devices.len());
    sqlx::query(
        "INSERT INTO contact_shares (id, account_id, peer_id, device_id)
         SELECT id, $1, $2, device FROM unnest($3::uuid[], $4::uuid[]) AS t(id, device)",
    )
    .bind(c.account_id)
    .bind(peer)
    .bind(device_ids)
    .bind(&devices)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO contact_shares (id, account_id, peer_id, agent_id)
         SELECT id, $1, $2, agent FROM unnest($3::uuid[], $4::uuid[]) AS t(id, agent)",
    )
    .bind(c.account_id)
    .bind(peer)
    .bind(agent_ids)
    .bind(&sessions)
    .execute(&mut *tx)
    .await?;
    let view = share_view(&mut tx, c.account_id, peer).await;
    tx.commit().await?;
    info!(
        account = c.account_id,
        peer,
        share,
        devices = devices.len(),
        sessions = sessions.len(),
        "contact share set"
    );
    Ok(Json(view?))
}
