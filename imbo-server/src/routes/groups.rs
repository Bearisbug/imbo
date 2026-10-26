//! API-018..022: groups.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::{Extension, Json};
use imbo_proto::ErrorCode;
use imbo_proto::api::{
    AccountRef, CreateGroupBody, GroupRef, GroupResponse, GroupView, GroupsResponse, InviteBody,
    MySessionsResponse, RequestItem, RequestResponse, SessionsBody,
};
use imbo_proto::text::validate_group_name;
use rand::Rng;
use uuid::Uuid;

use super::Body;
use crate::auth::Caller;
use crate::error::{ApiResult, err};
use crate::state::St;
use crate::{delivery, notify, views};

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

async fn group_name(st: &St, gid: i32) -> Result<Option<String>, sqlx::Error> {
    sqlx::query_scalar("SELECT name FROM chat_groups WHERE id = $1")
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

async fn view(st: &St, c: &Caller, g: GroupRef) -> ApiResult<GroupView> {
    let members: Vec<(i32, String)> = sqlx::query_as(
        "SELECT a.id, a.name FROM group_members m JOIN accounts a ON a.id = m.account_id
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
    Ok(GroupView {
        id: g.id,
        name: g.name,
        members: members
            .into_iter()
            .map(|(id, name)| AccountRef { id, name })
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
    sqlx::query("INSERT INTO group_members (group_id, account_id) VALUES ($1, $2)")
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
         WHERE m.account_id = $1 ORDER BY g.name, g.id",
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
    let mut tx = st.db.begin().await?;
    sqlx::query("DELETE FROM group_members WHERE group_id = $1 AND account_id = $2")
        .bind(g.id)
        .bind(c.account_id)
        .execute(&mut *tx)
        .await?;
    let canceled: Vec<(i64, Uuid)> = sqlx::query_as(
        "UPDATE deliveries d SET state = 'canceled', canceled_at = now(), cancel_reason = 'left_group'
         FROM messages m JOIN conversations cv ON cv.id = m.conversation_id
         WHERE d.message_id = m.id AND cv.group_id = $1 AND d.account_id = $2 AND d.state IN ('pending', 'pushed')
         RETURNING d.message_id, d.agent_id",
    )
    .bind(g.id)
    .bind(c.account_id)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    delivery::notify_canceled(&st, canceled).await;
    Ok(StatusCode::NO_CONTENT)
}
