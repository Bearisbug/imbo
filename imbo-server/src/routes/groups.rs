//! API-018..022, API-037..039, API-077, API-087..090: groups, their owner and
//! admins, finding and asking to join one, removing and dissolving.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::{Extension, Json};
use imbo_proto::api::{
    AccountRef, AgentRef, CreateGroupBody, GroupHit, GroupRef, GroupResponse, GroupSearchResponse,
    GroupView, GroupsResponse, InviteBody, MemberRef, MySessionsResponse, RequestItem,
    RequestResponse, SessionsBody,
};
use imbo_proto::text::validate_group_name;
use imbo_proto::{ErrorCode, MAX_NOTE_CHARS, TOOL_TERMINAL};
use rand::Rng;
use serde::Deserialize;
use sqlx::PgConnection;
use uuid::Uuid;

use super::Body;
use crate::auth::Caller;
use crate::error::{ApiError, ApiResult, err, rate_limited};
use crate::state::St;
use crate::{avatar, delivery, notify, push, safety, views};

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

pub(super) async fn require_member(st: &St, c: &Caller, raw: &str) -> ApiResult<GroupRef> {
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
pub(super) async fn role_locked(
    tx: &mut PgConnection,
    g: &GroupRef,
    account: i32,
) -> ApiResult<String> {
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
pub(super) async fn not_owner(
    tx: &mut PgConnection,
    g: &GroupRef,
    what: &str,
) -> ApiResult<ApiError> {
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

/// A notice from the owner into `to`'s inbox, in the two accounts' direct
/// conversation (ADR-028); `via` is where the owner did it, if they did.
/// Returns the inbox agent and the message.
pub(super) async fn post_notice(
    tx: &mut PgConnection,
    from: i32,
    via: Option<&str>,
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
        "INSERT INTO messages (conversation_id, from_account, from_via, to_agent_id, body, client_msg_id)
         VALUES ($1, $2, $3, $4, $5, $6) RETURNING id",
    )
    .bind(conversation)
    .bind(from)
    .bind(via)
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
    let (picture, discoverable): (Option<Uuid>, bool) =
        sqlx::query_as("SELECT avatar_id, discoverable FROM chat_groups WHERE id = $1")
            .bind(g.id)
            .fetch_one(&st.db)
            .await?;
    let members: Vec<(i32, String, String)> = sqlx::query_as(
        "SELECT a.id, a.name, m.role FROM group_members m JOIN accounts a ON a.id = m.account_id
         WHERE m.group_id = $1 ORDER BY m.joined_at",
    )
    .bind(g.id)
    .fetch_all(&st.db)
    .await?;
    // Everyone's picked sessions, listed to pick from when typing @ (API-019).
    let picked: Vec<(i32, Uuid)> =
        sqlx::query_as("SELECT account_id, agent_id FROM member_sessions WHERE group_id = $1")
            .bind(g.id)
            .fetch_all(&st.db)
            .await?;
    let all: Vec<Uuid> = picked.iter().map(|(_, a)| *a).collect();
    let refs = views::agent_refs(&st.db, &all).await?;
    let sessions_of = |account: i32| -> Vec<AgentRef> {
        refs.iter()
            .filter(|r| picked.contains(&(account, r.id)))
            .cloned()
            .collect()
    };
    let owner = members
        .iter()
        .find(|(_, _, role)| role == "owner")
        .map(|(id, name, _)| AccountRef {
            id: *id,
            name: name.clone(),
        });
    let ids: Vec<i32> = members.iter().map(|m| m.0).collect();
    let inactive = safety::inactive(&st.db, &st.cfg, &ids).await?;
    let mut avatars = safety::avatars(&st.db, &st.cfg, &ids).await?;
    Ok(GroupView {
        id: g.id,
        name: g.name,
        owner,
        members: members
            .into_iter()
            .map(|(id, name, role)| MemberRef {
                id,
                name,
                avatar_url: avatars.remove(&id),
                active: !inactive.contains(&id),
                sessions: sessions_of(id),
                role: Some(role),
            })
            .collect(),
        my_sessions: sessions_of(c.account_id),
        avatar_url: avatar::url_of(&st.cfg, picture),
        discoverable,
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
    let to = views::account_ref(&st.db, target)
        .await?
        .unwrap_or(AccountRef {
            id: target,
            name: String::new(),
        });
    let mut tx = st.db.begin().await?;
    // RULE-013, RULE-014, behind a block of me or a ban of either of us in
    // progress (DESIGN §16).
    let banned: bool =
        sqlx::query_scalar("SELECT banned_at IS NOT NULL FROM accounts WHERE id = $1 FOR SHARE")
            .bind(c.account_id)
            .fetch_one(&mut *tx)
            .await?;
    if banned {
        return Err(err(ErrorCode::Unauthorized, "This account was banned."));
    }
    safety::check_blocks(&mut tx, c.account_id, target, &to.name).await?;
    safety::check_not_banned(&mut tx, target, &to.name).await?;
    let pending: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM requests WHERE kind = 'group' AND group_id = $1 AND to_account = $2 AND status = 'pending')",
    )
    .bind(g.id)
    .bind(target)
    .fetch_one(&mut *tx)
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
    .fetch_one(&mut *tx)
    .await?;
    tx.commit().await?;
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
        to: Some(to),
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

/// API-037: the owner or an admin, or an agent of theirs, removes a member;
/// an admin only ordinary members (RULE-023).
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
    // RULE-008, RULE-014: an inactive account gets no notice (INV-014).
    let active = !safety::inactive(&st.db, &st.cfg, &[target])
        .await?
        .contains(&target);
    let mut tx = st.db.begin().await?;
    let mine = role_locked(&mut tx, &g, c.account_id).await?;
    if mine == "member" {
        return Err(err(
            ErrorCode::NotOwner,
            format!(
                "Only the owner and admins of {} (#{}) can remove members.",
                g.name, g.id
            ),
        ));
    }
    let theirs: Option<String> = sqlx::query_scalar(
        "SELECT role FROM group_members WHERE group_id = $1 AND account_id = $2",
    )
    .bind(g.id)
    .bind(target)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(theirs) = theirs else {
        return Err(missing());
    };
    if mine == "admin" && theirs != "member" {
        return Err(not_owner(&mut tx, &g, "remove the owner or an admin").await?);
    }
    sqlx::query("DELETE FROM group_members WHERE group_id = $1 AND account_id = $2")
        .bind(g.id)
        .bind(target)
        .execute(&mut *tx)
        .await?;
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
        notices.push(post_notice(&mut tx, c.account_id, Some(c.via()), target, body).await?);
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

#[derive(Deserialize)]
pub struct UpdateBody {
    name: Option<String>,
    discoverable: Option<bool>,
}

/// API-077: only the owner, in person, renames the group or lets anyone find
/// it. Members see the change next time they look; no notice goes out.
pub async fn rename(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
    Path(raw): Path<String>,
    Body(req): Body<UpdateBody>,
) -> ApiResult<Json<GroupResponse>> {
    c.require_person()?;
    let mut g = require_member(&st, &c, &raw).await?;
    if req.name.is_none() && req.discoverable.is_none() {
        return Err(err(
            ErrorCode::InvalidRequest,
            "Give a new name or whether anyone can find the group.",
        ));
    }
    let name = req
        .name
        .map(|n| validate_group_name(&n).map_err(|m| err(ErrorCode::InvalidRequest, m)))
        .transpose()?;
    let mut tx = st.db.begin().await?;
    if role_locked(&mut tx, &g, c.account_id).await? != "owner" {
        let what = if name.is_some() { "rename it" } else { "change who can find it" };
        return Err(not_owner(&mut tx, &g, what).await?);
    }
    if let Some(name) = name.filter(|n| *n != g.name) {
        sqlx::query("UPDATE chat_groups SET name = $2 WHERE id = $1")
            .bind(g.id)
            .bind(&name)
            .execute(&mut *tx)
            .await?;
        g.name = name;
    }
    if let Some(on) = req.discoverable {
        sqlx::query("UPDATE chat_groups SET discoverable = $2 WHERE id = $1")
            .bind(g.id)
            .bind(on)
            .execute(&mut *tx)
            .await?;
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
    // The group's picture goes with it (API-039).
    let picture: Option<Uuid> = sqlx::query_scalar(
        "UPDATE chat_groups SET dissolved_at = now() WHERE id = $1 RETURNING avatar_id",
    )
    .bind(g.id)
    .fetch_one(&mut *tx)
    .await?;
    avatar::delete(&mut tx, &picture.into_iter().collect::<Vec<_>>()).await?;
    let canceled = cancel_group(&mut tx, g.id, None, "group_dissolved").await?;
    let others: Vec<i32> = members.into_iter().filter(|a| *a != c.account_id).collect();
    // RULE-008, RULE-014: inactive members get no notice (INV-014).
    let inactive = safety::inactive(&st.db, &st.cfg, &others).await?;
    let mut notices = vec![];
    for to in others {
        if inactive.contains(&to) {
            continue;
        }
        let body = format!("Group {} (#{}) was dissolved by {}.", g.name, g.id, c.name);
        notices.push(post_notice(&mut tx, c.account_id, Some(c.via()), to, body).await?);
    }
    tx.commit().await?;
    delivery::notify_canceled(&st, canceled).await;
    let (inbox, messages): (Vec<Uuid>, Vec<i64>) = notices.into_iter().unzip();
    delivery::kick(&st, inbox);
    push::messages(&st, messages);
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct SearchQuery {
    q: Option<String>,
}

#[derive(sqlx::FromRow)]
struct HitRow {
    id: i32,
    name: String,
    avatar_id: Option<Uuid>,
    member_count: i64,
    owner_id: i32,
    owner_name: String,
    joined: bool,
    applied: bool,
}

/// API-087: groups anyone can find, by ID or by a piece of their name.
pub async fn search(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
    Query(q): Query<SearchQuery>,
) -> ApiResult<Json<GroupSearchResponse>> {
    c.require_person()?;
    let raw = q.q.unwrap_or_default();
    let q = raw.trim();
    if q.is_empty() || q.chars().count() > 64 {
        return Err(err(
            ErrorCode::InvalidRequest,
            "Search with a group ID or 1 to 64 characters of its name.",
        ));
    }
    let id: Option<i32> = q.trim_start_matches('#').parse().ok();
    let rows: Vec<HitRow> = sqlx::query_as(
        "SELECT g.id, g.name, g.avatar_id,
                (SELECT count(*) FROM group_members m WHERE m.group_id = g.id) AS member_count,
                o.id AS owner_id, o.name AS owner_name,
                EXISTS (SELECT 1 FROM group_members m WHERE m.group_id = g.id AND m.account_id = $3) AS joined,
                EXISTS (SELECT 1 FROM requests r WHERE r.kind = 'join' AND r.group_id = g.id
                        AND r.from_account = $3 AND r.status = 'pending') AS applied
         FROM chat_groups g
         JOIN group_members om ON om.group_id = g.id AND om.role = 'owner'
         JOIN accounts o ON o.id = om.account_id
         WHERE g.discoverable AND g.dissolved_at IS NULL
           AND (g.id = $1 OR strpos(lower(g.name), lower($2)) > 0)
         ORDER BY member_count DESC, g.id
         LIMIT 20",
    )
    .bind(id)
    .bind(q)
    .bind(c.account_id)
    .fetch_all(&st.db)
    .await?;
    Ok(Json(GroupSearchResponse {
        groups: rows
            .into_iter()
            .map(|r| GroupHit {
                id: r.id,
                name: r.name,
                avatar_url: avatar::url_of(&st.cfg, r.avatar_id),
                member_count: r.member_count,
                owner: AccountRef {
                    id: r.owner_id,
                    name: r.owner_name,
                },
                joined: r.joined,
                applied: r.applied,
            })
            .collect(),
    }))
}

#[derive(Deserialize)]
pub struct ApplyBody {
    note: Option<String>,
}

/// API-088: ask to join a group anyone can find; its owner and admins are
/// told (FLOW-GROUP-DISCOVER S5–S6).
pub async fn apply(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
    Path(raw): Path<String>,
    Body(req): Body<ApplyBody>,
) -> ApiResult<(StatusCode, Json<RequestResponse>)> {
    c.require_person()?;
    crate::billing::require_active(&st, c.account_id, "API-088").await?;
    let gid = parse_gid(&raw)?;
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
    let mut tx = st.db.begin().await?;
    lock_group(&mut tx, gid).await?;
    let name: Option<String> = sqlx::query_scalar(
        "SELECT name FROM chat_groups WHERE id = $1 AND discoverable AND dissolved_at IS NULL",
    )
    .bind(gid)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(name) = name else {
        return Err(err(
            ErrorCode::NotFound,
            format!("No group #{gid} that anyone can find."),
        ));
    };
    let (member, pending, today): (bool, bool, i64) = sqlx::query_as(
        "SELECT EXISTS (SELECT 1 FROM group_members WHERE group_id = $1 AND account_id = $2),
                EXISTS (SELECT 1 FROM requests WHERE kind = 'join' AND group_id = $1 AND from_account = $2 AND status = 'pending'),
                (SELECT count(*) FROM requests WHERE kind = 'join' AND from_account = $2 AND created_at > now() - interval '1 day')",
    )
    .bind(gid)
    .bind(c.account_id)
    .fetch_one(&mut *tx)
    .await?;
    if member {
        return Err(err(
            ErrorCode::AlreadyMember,
            format!("You are already in {name}."),
        ));
    }
    if pending {
        return Err(err(
            ErrorCode::RequestPending,
            format!("You already asked to join {name}. Its owner or an admin will answer."),
        ));
    }
    if today >= 20 {
        return Err(rate_limited(
            "join-requests",
            Some(c.account_id),
            "At most 20 requests to join groups per day.",
            3600,
        ));
    }
    let (id, created_at): (i64, chrono::DateTime<chrono::Utc>) = sqlx::query_as(
        "INSERT INTO requests (kind, from_account, group_id, note) VALUES ('join', $1, $2, $3)
         RETURNING id, created_at",
    )
    .bind(c.account_id)
    .bind(gid)
    .bind(&note)
    .fetch_one(&mut *tx)
    .await?;
    let handlers: Vec<i32> = sqlx::query_scalar(
        "SELECT account_id FROM group_members WHERE group_id = $1 AND role IN ('owner', 'admin')",
    )
    .bind(gid)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    for to in handlers {
        notify::send(
            &st,
            to,
            "join_request",
            format!(
                "{} ({}) asked to join group {name} (#{gid}). `imbo contact requests` to review.",
                c.name, c.account_id
            ),
            note.clone(),
            Some(id),
        );
    }
    let request = RequestItem {
        id,
        kind: "join".into(),
        from: AccountRef {
            id: c.account_id,
            name: c.name.clone(),
        },
        to: None,
        group: Some(GroupRef { id: gid, name }),
        note: None,
        status: "pending".into(),
        decided_by: None,
        created_at,
    };
    Ok((StatusCode::CREATED, Json(RequestResponse { request })))
}

/// API-089: the owner, in person, makes a member an admin.
pub async fn set_admin(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
    Path((raw, who)): Path<(String, String)>,
) -> ApiResult<Json<GroupResponse>> {
    admin(&st, &c, &raw, &who, true).await
}

/// API-090: the owner, in person, makes an admin an ordinary member again.
pub async fn unset_admin(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
    Path((raw, who)): Path<(String, String)>,
) -> ApiResult<Json<GroupResponse>> {
    admin(&st, &c, &raw, &who, false).await
}

/// Under the group lock, so it can't cross the member leaving (DESIGN §16).
async fn admin(st: &St, c: &Caller, raw: &str, who: &str, on: bool) -> ApiResult<Json<GroupResponse>> {
    c.require_person()?;
    let g = require_member(st, c, raw).await?;
    let missing = || err(ErrorCode::NotFound, format!("{who} is not in {}.", g.name));
    let target: i32 = who.parse().map_err(|_| missing())?;
    let mut tx = st.db.begin().await?;
    if role_locked(&mut tx, &g, c.account_id).await? != "owner" {
        return Err(not_owner(&mut tx, &g, "choose its admins").await?);
    }
    if target == c.account_id {
        return Err(err(
            ErrorCode::InvalidRequest,
            format!("You own {}; the owner isn't an admin too.", g.name),
        ));
    }
    let (from, to) = if on { ("member", "admin") } else { ("admin", "member") };
    let found: Option<String> = sqlx::query_scalar(
        "SELECT role FROM group_members WHERE group_id = $1 AND account_id = $2",
    )
    .bind(g.id)
    .bind(target)
    .fetch_optional(&mut *tx)
    .await?;
    if found.is_none() {
        return Err(missing());
    }
    sqlx::query(
        "UPDATE group_members SET role = $4 WHERE group_id = $1 AND account_id = $2 AND role = $3",
    )
    .bind(g.id)
    .bind(target)
    .bind(from)
    .bind(to)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    let group = view(st, c, g).await?;
    Ok(Json(GroupResponse { group }))
}
