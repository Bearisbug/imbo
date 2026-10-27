//! API-018..022, API-037..039: groups, their owner, removing and dissolving.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::{Extension, Json};
use imbo_proto::api::{
    AccountRef, CreateGroupBody, GroupRef, GroupResponse, GroupView, GroupsResponse, InviteBody,
    MySessionsResponse, RequestItem, RequestResponse, SessionsBody,
};
use imbo_proto::text::validate_group_name;
use imbo_proto::{ErrorCode, TOOL_TERMINAL};
use rand::Rng;
use serde::Deserialize;
use sqlx::PgConnection;
use uuid::Uuid;

use super::Body;
use crate::auth::Caller;
use crate::error::{ApiError, ApiResult, err};
use crate::state::St;
use crate::{billing, delivery, notify, push, views};

async fn owned_sessions(st: &St, c: &Caller, ids: &[Uuid]) -> ApiResult<Vec<Uuid>> {
    let mut want = ids.to_vec();
    want.sort();
    want.dedup();
    if want.is_empty() {
        return Ok(want);
    }
    let owned: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM agents WHERE account_id = $1 AND id = ANY($2) AND tool <> 'terminal'",
    )
    .bind(c.account_id)
    .bind(&want)
    .fetch_all(&st.db)
    .await?;
    if owned.len() != want.len() {
        return Err(err(
            ErrorCode::SessionNotFound,
            "Some of those sessions are not yours.",
        ));
    }
    Ok(owned)
}

async fn is_member(st: &St, gid: i32, account: i32) -> Result<bool, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM group_members WHERE group_id = $1 AND account_id = $2)",
    )
    .bind(gid)
    .bind(account)
    .fetch_one(&st.db)
    .await
}

/// A dissolved group counts as missing (ADR-029).
async fn group_name(st: &St, gid: i32) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar("SELECT name FROM chat_groups WHERE id = $1 AND dissolved_at IS NULL")
        .bind(gid)
        .fetch_optional(&st.db)
        .await
}

/// `{id}` may be written `5520183` or `#5520183`.
fn parse_gid(raw: &str) -> ApiResult<i32> {
    raw.trim_start_matches('#')
        .parse()
        .map_err(|_| err(ErrorCode::NotFound, "No such group."))
}

async fn require_member(st: &St, c: &Caller, raw: &str) -> ApiResult<GroupRef> {
    let gid = parse_gid(raw)?;
    let Some(name) = group_name(st, gid).await? else {
        return Err(err(ErrorCode::NotFound, format!("No group #{gid}.")));
    };
    if !is_member(st, gid, c.account_id).await? {
        return Err(err(
            ErrorCode::NotMember,
            format!("You are not in group {name} (#{gid})."),
        ));
    }
    Ok(GroupRef { id: gid, name })
}

/// The group lock: the group conversation's advisory lock, the one a group
/// send takes for its rate limit (DESIGN §16).
pub(super) async fn lock_group(tx: &mut PgConnection, gid: i32) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT pg_advisory_xact_lock(-id) FROM conversations WHERE group_id = $1")
        .bind(gid)
        .execute(&mut *tx)
        .await?;
    Ok(())
}

/// Take the group lock, then the account's role in the group.
async fn role_locked(tx: &mut PgConnection, g: &GroupRef, account: i32) -> ApiResult<String> {
    lock_group(tx, g.id).await?;
    let (dissolved, role): (bool, Option<String>) = sqlx::query_as(
        "SELECT g.dissolved_at IS NOT NULL,
                (SELECT role FROM group_members WHERE group_id = g.id AND account_id = $2)
         FROM chat_groups g WHERE g.id = $1",
    )
    .bind(g.id)
    .bind(account)
    .fetch_one(&mut *tx)
    .await?;
    if dissolved {
        return Err(err(ErrorCode::NotFound, format!("No group #{}.", g.id)));
    }
    role.ok_or_else(|| {
        err(
            ErrorCode::NotMember,
            format!("You are not in group {} (#{}).", g.name, g.id),
        )
    })
}

/// RULE-010, naming the owner.
async fn not_owner(tx: &mut PgConnection, g: &GroupRef, what: &str) -> ApiResult<ApiError> {
    let owner: Option<(i32, String)> = sqlx::query_as(
        "SELECT a.id, a.name FROM group_members m JOIN accounts a ON a.id = m.account_id
         WHERE m.group_id = $1 AND m.role = 'owner'",
    )
    .bind(g.id)
    .fetch_optional(&mut *tx)
    .await?;
    let who = owner
        .map(|(id, name)| format!(" The owner is {name} ({id})."))
        .unwrap_or_default();
    Ok(err(
        ErrorCode::NotOwner,
        format!("Only the owner of {} (#{}) can {what}.{who}", g.name, g.id),
    ))
}

/// Cancel the group's undelivered messages to `account`, or to everyone.
pub(super) async fn cancel_group(
    tx: &mut PgConnection,
    gid: i32,
    account: Option<i32>,
    reason: &str,
) -> Result<Vec<(i64, Uuid)>, sqlx::Error> {
    sqlx::query_as(
        "UPDATE deliveries d SET state = 'canceled', canceled_at = now(), cancel_reason = $3
         FROM messages m JOIN conversations cv ON cv.id = m.conversation_id
         WHERE d.message_id = m.id AND cv.group_id = $1 AND ($2::integer IS NULL OR d.account_id = $2)
           AND d.state IN ('pending', 'pushed')
         RETURNING d.message_id, d.agent_id",
    )
    .bind(gid)
    .bind(account)
    .bind(reason)
    .fetch_all(&mut *tx)
    .await
}

/// A notice from the owner's terminal into `to`'s terminal inbox, in the
/// two accounts' direct conversation (ADR-028). Returns the inbox agent and
/// the message.
pub(super) async fn post_notice(
    tx: &mut PgConnection,
    from: i32,
    to: i32,
    body: String,
) -> ApiResult<(Uuid, i64)> {
    let (lo, hi) = (from.min(to), from.max(to));
    sqlx::query(
        "INSERT INTO conversations (kind, account_lo, account_hi) VALUES ('direct', $1, $2) ON CONFLICT DO NOTHING",
    )
    .bind(lo)
    .bind(hi)
    .execute(&mut *tx)
    .await?;
    let conversation: i64 = sqlx::query_scalar(
        "SELECT id FROM conversations WHERE account_lo = $1 AND account_hi = $2",
    )
    .bind(lo)
    .bind(hi)
    .fetch_one(&mut *tx)
    .await?;
    // The terminal inbox row, as in messages::terminal_of (ADR-010).
    let inbox: Uuid = sqlx::query_scalar(
        "INSERT INTO agents (id, account_id, tool, session_id, short_id, title)
         VALUES ($1, $2, $3, $3, $3, $3)
         ON CONFLICT (account_id, tool, session_id) DO UPDATE SET online = false
         RETURNING id",
    )
    .bind(Uuid::now_v7())
    .bind(to)
    .bind(TOOL_TERMINAL)
    .fetch_one(&mut *tx)
    .await?;
    let message: i64 = sqlx::query_scalar(
        "INSERT INTO messages (conversation_id, from_account, to_agent_id, body, client_msg_id)
         VALUES ($1, $2, $3, $4, $5) RETURNING id",
    )
    .bind(conversation)
    .bind(from)
    .bind(inbox)
    .bind(body)
    .bind(format!("group-notice-{}", Uuid::now_v7()))
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query("INSERT INTO deliveries (message_id, agent_id, account_id) VALUES ($1, $2, $3)")
        .bind(message)
        .bind(inbox)
        .bind(to)
        .execute(&mut *tx)
        .await?;
    Ok((inbox, message))
}

async fn view(st: &St, c: &Caller, g: GroupRef) -> ApiResult<GroupView> {
    let members: Vec<(i32, String, String)> = sqlx::query_as(
        "SELECT a.id, a.name, m.role FROM group_members m JOIN accounts a ON a.id = m.account_id
         WHERE m.group_id = $1 ORDER BY m.joined_at",
    )
    .bind(g.id)
    .fetch_all(&st.db)
    .await?;
    let mine: Vec<Uuid> = sqlx::query_scalar(
        "SELECT agent_id FROM member_sessions WHERE group_id = $1 AND account_id = $2",
    )
    .bind(g.id)
    .bind(c.account_id)
    .fetch_all(&st.db)
    .await?;
    let owner = members
        .iter()
        .find(|(_, _, role)| role == "owner")
        .map(|(id, name, _)| AccountRef {
            id: *id,
            name: name.clone(),
        });
    Ok(GroupView {
        id: g.id,
        name: g.name,
        owner,
        members: members
            .into_iter()
            .map(|(id, name, _)| AccountRef { id, name })
            .collect(),
        my_sessions: views::agent_refs(&st.db, &mine).await?,
    })
}

pub async fn create(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
    Body(req): Body<CreateGroupBody>,
) -> ApiResult<(StatusCode, Json<GroupResponse>)> {
    crate::billing::require_active(&st, c.account_id, "API-018").await?;
    let name = validate_group_name(&req.name).map_err(|m| err(ErrorCode::InvalidRequest, m))?;
    let sessions = match req.sessions {
        Some(s) => owned_sessions(&st, &c, &s).await?,
        None => c.actor.as_ref().map(|a| vec![a.id]).unwrap_or_default(),
    };
    let mut tx = st.db.begin().await?;
    let mut gid = 0;
    for _ in 0..20 {
        let candidate = rand::rng().random_range(1_000_000..=9_999_999);
        let r = sqlx::query("INSERT INTO chat_groups (id, name, created_by) VALUES ($1, $2, $3) ON CONFLICT (id) DO NOTHING")
            .bind(candidate)
            .bind(&name)
            .bind(c.account_id)
            .execute(&mut *tx)
            .await?;
        if r.rows_affected() == 1 {
            gid = candidate;
            break;
        }
    }
    if gid == 0 {
        return Err(err(
            ErrorCode::Unavailable,
            "Could not allocate a group ID. Try again.",
        ));
    }
    sqlx::query("INSERT INTO conversations (kind, group_id) VALUES ('group', $1)")
        .bind(gid)
        .execute(&mut *tx)
        .await?;
    sqlx::query("INSERT INTO group_members (group_id, account_id, role) VALUES ($1, $2, 'owner')")
        .bind(gid)
        .bind(c.account_id)
        .execute(&mut *tx)
        .await?;
    for agent in &sessions {
        sqlx::query(
            "INSERT INTO member_sessions (group_id, account_id, agent_id) VALUES ($1, $2, $3)",
        )
        .bind(gid)
        .bind(c.account_id)
        .bind(agent)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    let group = view(&st, &c, GroupRef { id: gid, name }).await?;
    Ok((StatusCode::CREATED, Json(GroupResponse { group })))
}

pub async fn list(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
) -> ApiResult<Json<GroupsResponse>> {
    let rows: Vec<(i32, String)> = sqlx::query_as(
        "SELECT g.id, g.name FROM group_members m JOIN chat_groups g ON g.id = m.group_id
         WHERE m.account_id = $1 AND g.dissolved_at IS NULL ORDER BY g.name, g.id",
    )
    .bind(c.account_id)
    .fetch_all(&st.db)
    .await?;
    let mut groups = vec![];
    for (id, name) in rows {
        groups.push(view(&st, &c, GroupRef { id, name }).await?);
    }
    Ok(Json(GroupsResponse { groups }))
}

pub async fn invite(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
    Path(raw): Path<String>,
    Body(req): Body<InviteBody>,
) -> ApiResult<(StatusCode, Json<RequestResponse>)> {
    crate::billing::require_active(&st, c.account_id, "API-020").await?;
    let g = require_member(&st, &c, &raw).await?;
    let target = req.account_id;
    let contact: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM contacts WHERE account_id = $1 AND peer_id = $2)",
    )
    .bind(c.account_id)
    .bind(target)
    .fetch_one(&st.db)
    .await?;
    if !contact {
        return Err(err(
            ErrorCode::NotContact,
            format!("{target} is not your contact. Only contacts can be invited."),
        ));
    }
    if is_member(&st, g.id, target).await? {
        return Err(err(
            ErrorCode::AlreadyMember,
            format!("{target} is already in {}.", g.name),
        ));
    }
    // RULE-009: someone the owner removed comes back only on the owner's invite.
    let (removed, owner): (bool, bool) = sqlx::query_as(
        "SELECT EXISTS (SELECT 1 FROM group_removals WHERE group_id = $1 AND account_id = $2),
                EXISTS (SELECT 1 FROM group_members WHERE group_id = $1 AND account_id = $3 AND role = 'owner')",
    )
    .bind(g.id)
    .bind(target)
    .bind(c.account_id)
    .fetch_one(&st.db)
    .await?;
    if removed && !owner {
        return Err(err(
            ErrorCode::RemovedFromGroup,
            format!(
                "{target} was removed from {} by its owner. Only the owner can invite them back.",
                g.name
            ),
        ));
    }
    let pending: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM requests WHERE kind = 'group' AND group_id = $1 AND to_account = $2 AND status = 'pending')",
    )
    .bind(g.id)
    .bind(target)
    .fetch_one(&st.db)
    .await?;
    if pending {
        return Err(err(
            ErrorCode::RequestPending,
            "They already have a pending invite to this group.",
        ));
    }
    let id: i64 = sqlx::query_scalar(
        "INSERT INTO requests (kind, from_account, to_account, group_id) VALUES ('group', $1, $2, $3) RETURNING id",
    )
    .bind(c.account_id)
    .bind(target)
    .bind(g.id)
    .fetch_one(&st.db)
    .await?;
    let to = views::account_ref(&st.db, target)
        .await?
        .unwrap_or(AccountRef {
            id: target,
            name: String::new(),
        });
    notify::send(
        &st,
        target,
        "group_invite",
        format!(
            "{} ({}) invited you to group {} (#{}). `imbo group join #{}` to join.",
            c.name, c.account_id, g.name, g.id, g.id
        ),
        None,
        Some(id),
    );
    let request = RequestItem {
        id,
        kind: "group".into(),
        from: AccountRef {
            id: c.account_id,
            name: c.name.clone(),
        },
        to,
        group: Some(g),
        note: None,
        status: "pending".into(),
        decided_by: None,
        created_at: chrono::Utc::now(),
    };
    Ok((StatusCode::CREATED, Json(RequestResponse { request })))
}

pub async fn set_sessions(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
    Path(raw): Path<String>,
    Body(req): Body<SessionsBody>,
) -> ApiResult<Json<MySessionsResponse>> {
    let g = require_member(&st, &c, &raw).await?;
    let wanted = owned_sessions(&st, &c, &req.sessions).await?;
    let mut tx = st.db.begin().await?;
    sqlx::query("DELETE FROM member_sessions WHERE group_id = $1 AND account_id = $2 AND NOT (agent_id = ANY($3))")
        .bind(g.id)
        .bind(c.account_id)
        .bind(&wanted)
        .execute(&mut *tx)
        .await?;
    for agent in &wanted {
        sqlx::query("INSERT INTO member_sessions (group_id, account_id, agent_id) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING")
            .bind(g.id)
            .bind(c.account_id)
            .bind(agent)
            .execute(&mut *tx)
            .await?;
    }
    let canceled: Vec<(i64, Uuid)> = sqlx::query_as(
        "UPDATE deliveries d SET state = 'canceled', canceled_at = now(), cancel_reason = 'session_unselected'
         FROM messages m JOIN conversations cv ON cv.id = m.conversation_id
         WHERE d.message_id = m.id AND cv.group_id = $1 AND d.account_id = $2
           AND NOT (d.agent_id = ANY($3)) AND d.state IN ('pending', 'pushed')
         RETURNING d.message_id, d.agent_id",
    )
    .bind(g.id)
    .bind(c.account_id)
    .bind(&wanted)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    delivery::notify_canceled(&st, canceled).await;
    Ok(Json(MySessionsResponse {
        my_sessions: views::agent_refs(&st.db, &wanted).await?,
    }))
}

pub async fn leave(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
    Path(raw): Path<String>,
) -> ApiResult<StatusCode> {
    let g = require_member(&st, &c, &raw).await?;
    leave_group(&st, &c, &g).await
}

/// API-022, also API-037 on yourself. Under the group lock, so a group send
/// either reaches the leaver and is canceled here, or skips them (INV-020).
async fn leave_group(st: &St, c: &Caller, g: &GroupRef) -> ApiResult<StatusCode> {
    let mut tx = st.db.begin().await?;
    if role_locked(&mut tx, g, c.account_id).await? == "owner" {
        let others: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM group_members WHERE group_id = $1 AND account_id <> $2",
        )
        .bind(g.id)
        .bind(c.account_id)
        .fetch_one(&mut *tx)
        .await?;
        let detail = if others > 0 {
            format!(
                "You own {} (#{}). Make someone else the owner first: imbo group transfer #{} <account ID>.",
                g.name, g.id, g.id
            )
        } else {
            format!(
                "You're the only one left in {} (#{}). Dissolve it instead: imbo group dissolve #{}.",
                g.name, g.id, g.id
            )
        };
        return Err(err(ErrorCode::OwnerCannotLeave, detail));
    }
    sqlx::query("DELETE FROM group_members WHERE group_id = $1 AND account_id = $2")
        .bind(g.id)
        .bind(c.account_id)
        .execute(&mut *tx)
        .await?;
    let canceled = cancel_group(&mut tx, g.id, Some(c.account_id), "left_group").await?;
    tx.commit().await?;
    delivery::notify_canceled(st, canceled).await;
    Ok(StatusCode::NO_CONTENT)
}

/// API-037: the owner, or an agent of theirs, removes a member.
pub async fn remove(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
    Path((raw, who)): Path<(String, String)>,
) -> ApiResult<StatusCode> {
    let g = require_member(&st, &c, &raw).await?;
    let missing = || err(ErrorCode::NotFound, format!("{who} is not in {}.", g.name));
    let target: i32 = who.parse().map_err(|_| missing())?;
    if target == c.account_id {
        return leave_group(&st, &c, &g).await;
    }
    // RULE-008: an inactive account gets no notice (INV-014).
    let active = billing::access(&st.db, &st.cfg, target).await?.active();
    let mut tx = st.db.begin().await?;
    if role_locked(&mut tx, &g, c.account_id).await? != "owner" {
        return Err(not_owner(&mut tx, &g, "remove members").await?);
    }
    let gone = sqlx::query("DELETE FROM group_members WHERE group_id = $1 AND account_id = $2")
        .bind(g.id)
        .bind(target)
        .execute(&mut *tx)
        .await?;
    if gone.rows_affected() == 0 {
        return Err(missing());
    }
    sqlx::query(
        "INSERT INTO group_removals (group_id, account_id) VALUES ($1, $2)
         ON CONFLICT (group_id, account_id) DO UPDATE SET removed_at = now()",
    )
    .bind(g.id)
    .bind(target)
    .execute(&mut *tx)
    .await?;
    let canceled = cancel_group(&mut tx, g.id, Some(target), "removed_from_group").await?;
    let mut notices = vec![];
    if active {
        let body = format!(
            "You were removed from group {} (#{}) by {}.",
            g.name, g.id, c.name
        );
        notices.push(post_notice(&mut tx, c.account_id, target, body).await?);
    }
    tx.commit().await?;
    delivery::notify_canceled(&st, canceled).await;
    let (inbox, messages): (Vec<Uuid>, Vec<i64>) = notices.into_iter().unzip();
    delivery::kick(&st, inbox);
    push::messages(&st, messages);
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct OwnerBody {
    account_id: i32,
}

/// API-038: only the owner, in person, hands the group to another member.
pub async fn transfer(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
    Path(raw): Path<String>,
    Body(req): Body<OwnerBody>,
) -> ApiResult<Json<GroupResponse>> {
    c.require_person()?;
    let g = require_member(&st, &c, &raw).await?;
    let mut tx = st.db.begin().await?;
    if role_locked(&mut tx, &g, c.account_id).await? != "owner" {
        return Err(not_owner(&mut tx, &g, "make someone else the owner").await?);
    }
    if req.account_id != c.account_id {
        // At most one owner per group: step down first.
        sqlx::query(
            "UPDATE group_members SET role = 'member' WHERE group_id = $1 AND account_id = $2",
        )
        .bind(g.id)
        .bind(c.account_id)
        .execute(&mut *tx)
        .await?;
        let made = sqlx::query(
            "UPDATE group_members SET role = 'owner' WHERE group_id = $1 AND account_id = $2",
        )
        .bind(g.id)
        .bind(req.account_id)
        .execute(&mut *tx)
        .await?;
        if made.rows_affected() == 0 {
            return Err(err(
                ErrorCode::NotFound,
                format!(
                    "{} is not in {}. Invite them first.",
                    req.account_id, g.name
                ),
            ));
        }
    }
    tx.commit().await?;
    let group = view(&st, &c, g).await?;
    Ok(Json(GroupResponse { group }))
}

/// API-039: only the owner, in person, dissolves the group.
pub async fn dissolve(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
    Path(raw): Path<String>,
) -> ApiResult<StatusCode> {
    c.require_person()?;
    let g = require_member(&st, &c, &raw).await?;
    let mut tx = st.db.begin().await?;
    if role_locked(&mut tx, &g, c.account_id).await? != "owner" {
        return Err(not_owner(&mut tx, &g, "dissolve it").await?);
    }
    let members: Vec<i32> =
        sqlx::query_scalar("DELETE FROM group_members WHERE group_id = $1 RETURNING account_id")
            .bind(g.id)
            .fetch_all(&mut *tx)
            .await?;
    sqlx::query("UPDATE chat_groups SET dissolved_at = now() WHERE id = $1")
        .bind(g.id)
        .execute(&mut *tx)
        .await?;
    let canceled = cancel_group(&mut tx, g.id, None, "group_dissolved").await?;
    let others: Vec<i32> = members.into_iter().filter(|a| *a != c.account_id).collect();
    // RULE-008: inactive members get no notice (INV-014).
    let access = billing::access_many(&st.db, &st.cfg, &others).await?;
    let mut notices = vec![];
    for to in others {
        if access.get(&to).is_some_and(|a| !a.active()) {
            continue;
        }
        let body = format!("Group {} (#{}) was dissolved by {}.", g.name, g.id, c.name);
        notices.push(post_notice(&mut tx, c.account_id, to, body).await?);
    }
    tx.commit().await?;
    delivery::notify_canceled(&st, canceled).await;
    let (inbox, messages): (Vec<Uuid>, Vec<i64>) = notices.into_iter().unzip();
    delivery::kick(&st, inbox);
    push::messages(&st, messages);
    Ok(StatusCode::NO_CONTENT)
}
