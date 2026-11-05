//! API-030 listConversations, API-031 listConversationMessages,
//! API-036 listAgentMessages, API-078 markConversationRead,
//! API-085 setConversationPrefs, API-086 searchConversation: reading
//! history, for the web app and the App.

use axum::extract::{Path, Query, State};
use axum::{Extension, Json};
use imbo_proto::ErrorCode;
use imbo_proto::api::{
    ConversationItem, ConversationPrefs, ConversationsResponse, FileView, LastMessage, MessageView,
    MessagesPage, Profile, ReadResponse,
};
use serde::Deserialize;
use sqlx::PgPool;
use uuid::Uuid;

use super::Body;
use super::messages::ReadIn;
use crate::auth::Caller;
use crate::error::{ApiResult, err};
use crate::state::St;
use crate::{avatar, safety, views};

/// The first 100 characters of the body; the file names when a message
/// carries only files.
fn preview(body: &str, files: &[FileView]) -> String {
    let text = if body.is_empty() {
        files
            .iter()
            .map(|f| f.name.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    } else {
        body.to_string()
    };
    text.chars().take(100).collect()
}

#[derive(sqlx::FromRow)]
struct ConvRow {
    id: i64,
    kind: String,
    group_id: Option<i32>,
    group_name: Option<String>,
    group_avatar: Option<Uuid>,
    peer_id: Option<i32>,
    peer_name: Option<String>,
    last_id: Option<i64>,
    pending: i64,
    pinned: bool,
    marked_unread: bool,
    muted: bool,
}

pub async fn list(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
) -> ApiResult<Json<ConversationsResponse>> {
    c.require_person()?;
    let rows: Vec<ConvRow> = sqlx::query_as(
        "WITH mine AS (
           SELECT cv.id, cv.kind, cv.group_id,
                  CASE WHEN cv.account_lo = $1 THEN cv.account_hi ELSE cv.account_lo END AS peer_id
           FROM conversations cv
           WHERE (cv.kind = 'direct' AND $1 IN (cv.account_lo, cv.account_hi)
                  AND cv.account_lo <> cv.account_hi)
              OR cv.group_id IN (SELECT group_id FROM group_members WHERE account_id = $1)
         )
         SELECT mine.id, mine.kind, mine.group_id, g.name AS group_name, g.avatar_id AS group_avatar,
                mine.peer_id, p.name AS peer_name,
                (SELECT max(m.id) FROM messages m WHERE m.conversation_id = mine.id
                   AND (mine.kind = 'direct' OR m.from_account NOT IN
                        (SELECT blocked_id FROM blocks WHERE account_id = $1))) AS last_id,
                (SELECT count(*) FROM deliveries d JOIN messages m ON m.id = d.message_id
                 JOIN agents t ON t.id = d.agent_id
                 WHERE m.conversation_id = mine.id AND d.account_id = $1 AND t.tool = 'terminal'
                   AND d.state IN ('pending', 'pushed')) AS pending,
                coalesce(cp.pinned, false) AS pinned, coalesce(cp.marked_unread, false) AS marked_unread,
                coalesce(cp.muted, false) AS muted
         FROM mine
         LEFT JOIN chat_groups g ON g.id = mine.group_id
         LEFT JOIN accounts p ON p.id = mine.peer_id
         LEFT JOIN conversation_prefs cp ON cp.account_id = $1 AND cp.conversation_id = mine.id",
    )
    .bind(c.account_id)
    .fetch_all(&st.db)
    .await?;
    let mut rows: Vec<ConvRow> = rows
        .into_iter()
        .filter(|r| r.kind == "group" || r.last_id.is_some())
        .collect();
    // Pinned first, then newest last message first; message ids grow with
    // time.
    rows.sort_by(|a, b| {
        (b.pinned, b.last_id)
            .cmp(&(a.pinned, a.last_id))
            .then(b.id.cmp(&a.id))
    });
    rows.truncate(200);
    let last_ids: Vec<i64> = rows.iter().filter_map(|r| r.last_id).collect();
    let mut last = views::messages(&st.db, &last_ids, c.account_id).await?;
    let peers: Vec<i32> = rows.iter().filter_map(|r| r.peer_id).collect();
    let mut avatars = safety::avatars(&st.db, &st.cfg, &peers).await?;
    let conversations = rows
        .into_iter()
        .map(|r| {
            let last_message = r
                .last_id
                .and_then(|id| last.iter().position(|m| m.id == id))
                .map(|i| last.swap_remove(i))
                .map(|m| LastMessage {
                    id: m.id,
                    from: m.from,
                    preview: preview(&m.body, &m.files),
                    created_at: m.created_at,
                });
            ConversationItem {
                id: r.id,
                peer: match (r.kind.as_str(), r.peer_id, r.peer_name) {
                    ("direct", Some(id), Some(name)) => Some(Profile {
                        id,
                        name,
                        avatar_url: avatars.remove(&id),
                    }),
                    _ => None,
                },
                group: match (r.group_id, r.group_name) {
                    (Some(id), Some(name)) => Some(Profile {
                        id,
                        name,
                        avatar_url: avatar::url_of(&st.cfg, r.group_avatar),
                    }),
                    _ => None,
                },
                kind: r.kind,
                last_message,
                pending: r.pending,
                pinned: r.pinned,
                marked_unread: r.marked_unread,
                muted: r.muted,
            }
        })
        .collect();
    Ok(Json(ConversationsResponse { conversations }))
}

#[derive(Deserialize)]
pub struct PageQuery {
    before: Option<i64>,
    after: Option<i64>,
    limit: Option<i64>,
}

/// Message ids of one page, oldest first: of a conversation, or sent by or
/// delivered to a session.
/// `hide_blocked_by`: leave out messages from accounts this one blocks
/// (RULE-013, group conversations).
async fn page(
    st: &St,
    conv: Option<i64>,
    agent: Option<Uuid>,
    hide_blocked_by: Option<i32>,
    q: &PageQuery,
) -> ApiResult<(Vec<i64>, bool)> {
    let limit = q.limit.unwrap_or(50);
    if !(1..=100).contains(&limit) {
        return Err(err(ErrorCode::InvalidRequest, "limit must be 1 to 100."));
    }
    if q.before.is_some() && q.after.is_some() {
        return Err(err(
            ErrorCode::InvalidRequest,
            "Use before or after, not both.",
        ));
    }
    let order = if q.after.is_some() { "ASC" } else { "DESC" };
    let sql = format!(
        "SELECT m.id FROM messages m
         WHERE ($1::bigint IS NULL OR m.conversation_id = $1)
           AND ($2::uuid IS NULL OR m.from_agent_id = $2
                OR m.id IN (SELECT message_id FROM deliveries WHERE agent_id = $2))
           AND ($3::bigint IS NULL OR m.id < $3) AND ($4::bigint IS NULL OR m.id > $4)
           AND ($6::integer IS NULL OR m.from_account NOT IN
                (SELECT blocked_id FROM blocks WHERE account_id = $6))
         ORDER BY m.id {order} LIMIT $5"
    );
    let mut ids: Vec<i64> = sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
        .bind(conv)
        .bind(agent)
        .bind(q.before)
        .bind(q.after)
        .bind(limit + 1)
        .bind(hide_blocked_by)
        .fetch_all(&st.db)
        .await?;
    let has_more = ids.len() as i64 > limit;
    ids.truncate(limit as usize);
    ids.sort_unstable();
    Ok((ids, has_more))
}

/// The kind of conversation `id` when `me` may read it: one side of the
/// direct conversation, or in the group now.
async fn kind_of(st: &St, id: i64, me: i32) -> ApiResult<String> {
    let kind: Option<String> = sqlx::query_scalar(
        "SELECT cv.kind FROM conversations cv WHERE cv.id = $1 AND (
           $2 IN (cv.account_lo, cv.account_hi)
           OR EXISTS (SELECT 1 FROM group_members gm WHERE gm.group_id = cv.group_id AND gm.account_id = $2))",
    )
    .bind(id)
    .bind(me)
    .fetch_optional(&st.db)
    .await?;
    kind.ok_or_else(|| err(ErrorCode::NotFound, format!("No conversation {id}.")))
}

/// On messages that reached `me`'s sessions but none of them has taken
/// yet, the slowest delivery's state, for the "waiting for the session"
/// mark (API-031).
async fn with_waiting(
    db: &PgPool,
    messages: &mut [MessageView],
    me: i32,
) -> Result<(), sqlx::Error> {
    let ids: Vec<i64> = messages.iter().map(|m| m.id).collect();
    let states: Vec<(i64, String)> = sqlx::query_as(
        "SELECT d.message_id, min(d.state) FROM deliveries d JOIN agents a ON a.id = d.agent_id
         WHERE d.account_id = $1 AND a.tool <> 'terminal' AND d.message_id = ANY($2)
           AND d.state <> 'canceled'
         GROUP BY d.message_id HAVING bool_and(d.state IN ('pending', 'pushed'))",
    )
    .bind(me)
    .bind(&ids)
    .fetch_all(db)
    .await?;
    for (id, state) in states {
        if let Some(m) = messages.iter_mut().find(|m| m.id == id) {
            m.state = Some(state);
        }
    }
    Ok(())
}

pub async fn messages(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
    Path(id): Path<i64>,
    Query(q): Query<PageQuery>,
) -> ApiResult<Json<MessagesPage>> {
    c.require_person()?;
    let kind = kind_of(&st, id, c.account_id).await?;
    let hide = (kind == "group").then_some(c.account_id);
    let (ids, has_more) = page(&st, Some(id), None, hide, &q).await?;
    let mut messages = views::messages(&st.db, &ids, c.account_id).await?;
    with_waiting(&st.db, &mut messages, c.account_id).await?;
    Ok(Json(MessagesPage { messages, has_more }))
}

/// API-078: what the web app or the App has shown of a conversation counts
/// as read in my inbox, as `imbo inbox` would (REQ-104).
pub async fn mark_read(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
    Path(id): Path<i64>,
    Body(req): Body<ReadIn>,
) -> ApiResult<Json<ReadResponse>> {
    let Some(up_to) = req.up_to else {
        return Err(err(ErrorCode::InvalidRequest, "up_to is required."));
    };
    kind_of(&st, id, c.account_id).await?;
    let marked = sqlx::query(
        "UPDATE deliveries d SET state = 'delivered', delivered_at = now(), via = $3
         FROM messages m, agents t
         WHERE m.id = d.message_id AND t.id = d.agent_id
           AND d.account_id = $1 AND t.tool = 'terminal'
           AND m.conversation_id = $2 AND m.id <= $4
           AND d.state IN ('pending', 'pushed')",
    )
    .bind(c.account_id)
    .bind(id)
    .bind(c.via())
    .bind(up_to)
    .execute(&st.db)
    .await?
    .rows_affected();
    sqlx::query(
        "UPDATE conversation_prefs SET marked_unread = false
         WHERE account_id = $1 AND conversation_id = $2 AND marked_unread",
    )
    .bind(c.account_id)
    .bind(id)
    .execute(&st.db)
    .await?;
    Ok(Json(ReadResponse { marked }))
}

#[derive(Deserialize)]
pub struct PrefsIn {
    pinned: Option<bool>,
    marked_unread: Option<bool>,
    muted: Option<bool>,
}

/// API-085: my own pin, unread mark and mute for a conversation in my list;
/// only what the request gives changes.
pub async fn prefs(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
    Path(id): Path<i64>,
    Body(req): Body<PrefsIn>,
) -> ApiResult<Json<ConversationPrefs>> {
    c.require_person()?;
    if req.pinned.is_none() && req.marked_unread.is_none() && req.muted.is_none() {
        return Err(err(
            ErrorCode::InvalidRequest,
            "Give pinned, marked_unread or muted.",
        ));
    }
    // The conversations API-030 lists: not the one with myself.
    let listed: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM conversations cv WHERE cv.id = $1 AND (
           (cv.kind = 'direct' AND $2 IN (cv.account_lo, cv.account_hi) AND cv.account_lo <> cv.account_hi)
           OR EXISTS (SELECT 1 FROM group_members gm WHERE gm.group_id = cv.group_id AND gm.account_id = $2)))",
    )
    .bind(id)
    .bind(c.account_id)
    .fetch_one(&st.db)
    .await?;
    if !listed {
        return Err(err(ErrorCode::NotFound, format!("No conversation {id}.")));
    }
    let (pinned, marked_unread, muted) = sqlx::query_as(
        "INSERT INTO conversation_prefs (account_id, conversation_id, pinned, marked_unread, muted)
         VALUES ($1, $2, coalesce($3, false), coalesce($4, false), coalesce($5, false))
         ON CONFLICT (account_id, conversation_id) DO UPDATE SET
           pinned = coalesce($3, conversation_prefs.pinned),
           marked_unread = coalesce($4, conversation_prefs.marked_unread),
           muted = coalesce($5, conversation_prefs.muted)
         RETURNING pinned, marked_unread, muted",
    )
    .bind(c.account_id)
    .bind(id)
    .bind(req.pinned)
    .bind(req.marked_unread)
    .bind(req.muted)
    .fetch_one(&st.db)
    .await?;
    Ok(Json(ConversationPrefs {
        pinned,
        marked_unread,
        muted,
    }))
}

/// What API-086 returns at most.
const FOUND: i64 = 50;

#[derive(Deserialize)]
pub struct SearchQuery {
    q: Option<String>,
}

/// API-086: messages here whose body has `q` in it, any case, newest first;
/// in a group not the ones from people I block, as API-031.
pub async fn search(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
    Path(id): Path<i64>,
    Query(q): Query<SearchQuery>,
) -> ApiResult<Json<MessagesPage>> {
    c.require_person()?;
    let q = q.q.unwrap_or_default();
    if q.trim().is_empty() || q.chars().count() > 100 {
        return Err(err(
            ErrorCode::InvalidRequest,
            "q must be 1 to 100 characters.",
        ));
    }
    let kind = kind_of(&st, id, c.account_id).await?;
    let hide = (kind == "group").then_some(c.account_id);
    let like = format!(
        "%{}%",
        q.replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_")
    );
    let mut ids: Vec<i64> = sqlx::query_scalar(
        "SELECT m.id FROM messages m
         WHERE m.conversation_id = $1 AND m.body ILIKE $2
           AND ($3::integer IS NULL OR m.from_account NOT IN
                (SELECT blocked_id FROM blocks WHERE account_id = $3))
         ORDER BY m.id DESC LIMIT $4",
    )
    .bind(id)
    .bind(like)
    .bind(hide)
    .bind(FOUND + 1)
    .fetch_all(&st.db)
    .await?;
    let has_more = ids.len() as i64 > FOUND;
    ids.truncate(FOUND as usize);
    let mut messages = views::messages(&st.db, &ids, c.account_id).await?;
    messages.reverse();
    Ok(Json(MessagesPage { messages, has_more }))
}

pub async fn agent_messages(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
    Path(raw): Path<String>,
    Query(q): Query<PageQuery>,
) -> ApiResult<Json<MessagesPage>> {
    c.require_person()?;
    let not_found = || err(ErrorCode::NotFound, "No such session.");
    let agent = Uuid::parse_str(&raw).map_err(|_| not_found())?;
    // Only sessions API-014 lists: mine, not the terminal inbox, seen in 7 days.
    let listed: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM agents WHERE id = $1 AND account_id = $2 AND tool <> 'terminal'
           AND (online OR last_online_at > now() - interval '7 days'))",
    )
    .bind(agent)
    .bind(c.account_id)
    .fetch_one(&st.db)
    .await?;
    if !listed {
        return Err(not_found());
    }
    let (ids, has_more) = page(&st, None, Some(agent), None, &q).await?;
    let mut messages = views::messages(&st.db, &ids, c.account_id).await?;
    super::messages::with_states(&st.db, &mut messages, agent).await?;
    Ok(Json(MessagesPage { messages, has_more }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(name: &str) -> FileView {
        FileView {
            id: Uuid::new_v4(),
            name: name.into(),
            size: 1,
            sha256: String::new(),
            expires_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn preview_falls_back_to_file_names() {
        assert_eq!(preview("hi", &[file("a.txt")]), "hi");
        assert_eq!(preview("", &[file("a.txt"), file("b.png")]), "a.txt, b.png");
        assert_eq!(preview("", &[]), "");
        assert_eq!(preview(&"x".repeat(150), &[]).chars().count(), 100);
    }
}
