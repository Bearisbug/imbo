//! API-030 listConversations, API-031 listConversationMessages,
//! API-036 listAgentMessages: reading history, for the web app.

use axum::extract::{Path, Query, State};
use axum::{Extension, Json};
use imbo_proto::ErrorCode;
use imbo_proto::api::{
    AccountRef, ConversationItem, ConversationsResponse, FileView, GroupRef, LastMessage,
    MessagesPage,
};
use serde::Deserialize;
use uuid::Uuid;

use crate::auth::Caller;
use crate::error::{ApiResult, err};
use crate::state::St;
use crate::views;

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
    peer_id: Option<i32>,
    peer_name: Option<String>,
    last_id: Option<i64>,
    pending: i64,
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
           WHERE (cv.kind = 'direct' AND $1 IN (cv.account_lo, cv.account_hi))
              OR cv.group_id IN (SELECT group_id FROM group_members WHERE account_id = $1)
         )
         SELECT mine.id, mine.kind, mine.group_id, g.name AS group_name, mine.peer_id, p.name AS peer_name,
                (SELECT max(m.id) FROM messages m WHERE m.conversation_id = mine.id) AS last_id,
                (SELECT count(*) FROM deliveries d JOIN messages m ON m.id = d.message_id
                 WHERE m.conversation_id = mine.id AND d.account_id = $1
                   AND d.state IN ('pending', 'pushed')) AS pending
         FROM mine
         LEFT JOIN chat_groups g ON g.id = mine.group_id
         LEFT JOIN accounts p ON p.id = mine.peer_id",
    )
    .bind(c.account_id)
    .fetch_all(&st.db)
    .await?;
    let mut rows: Vec<ConvRow> = rows
        .into_iter()
        .filter(|r| r.kind == "group" || r.last_id.is_some())
        .collect();
    // Newest last message first; message ids grow with time.
    rows.sort_by(|a, b| b.last_id.cmp(&a.last_id).then(b.id.cmp(&a.id)));
    rows.truncate(200);
    let last_ids: Vec<i64> = rows.iter().filter_map(|r| r.last_id).collect();
    let mut last = views::messages(&st.db, &last_ids, c.account_id).await?;
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
                    ("direct", Some(id), Some(name)) => Some(AccountRef { id, name }),
                    _ => None,
                },
                group: match (r.group_id, r.group_name) {
                    (Some(id), Some(name)) => Some(GroupRef { id, name }),
                    _ => None,
                },
                kind: r.kind,
                last_message,
                pending: r.pending,
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
async fn page(
    st: &St,
    conv: Option<i64>,
    agent: Option<Uuid>,
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
         ORDER BY m.id {order} LIMIT $5"
    );
    let mut ids: Vec<i64> = sqlx::query_scalar(sqlx::AssertSqlSafe(sql))
        .bind(conv)
        .bind(agent)
        .bind(q.before)
        .bind(q.after)
        .bind(limit + 1)
        .fetch_all(&st.db)
        .await?;
    let has_more = ids.len() as i64 > limit;
    ids.truncate(limit as usize);
    ids.sort_unstable();
    Ok((ids, has_more))
}

pub async fn messages(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
    Path(id): Path<i64>,
    Query(q): Query<PageQuery>,
) -> ApiResult<Json<MessagesPage>> {
    c.require_person()?;
    let visible: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM conversations cv WHERE cv.id = $1 AND (
           $2 IN (cv.account_lo, cv.account_hi)
           OR EXISTS (SELECT 1 FROM group_members gm WHERE gm.group_id = cv.group_id AND gm.account_id = $2)))",
    )
    .bind(id)
    .bind(c.account_id)
    .fetch_one(&st.db)
    .await?;
    if !visible {
        return Err(err(ErrorCode::NotFound, format!("No conversation {id}.")));
    }
    let (ids, has_more) = page(&st, Some(id), None, &q).await?;
    let messages = views::messages(&st.db, &ids, c.account_id).await?;
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
    let (ids, has_more) = page(&st, None, Some(agent), &q).await?;
    let messages = views::messages(&st.db, &ids, c.account_id).await?;
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
