//! Shared read queries that turn rows into wire views.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use imbo_proto::api::{
    AccountRef, AgentRef, ConversationRef, FileView, GroupRef, MessageView, Sender,
};
use sqlx::PgPool;
use uuid::Uuid;

#[derive(sqlx::FromRow)]
struct MessageRow {
    id: i64,
    body: String,
    created_at: DateTime<Utc>,
    conversation_id: i64,
    kind: String,
    group_id: Option<i32>,
    group_name: Option<String>,
    from_account: i32,
    from_name: String,
    from_agent_id: Option<Uuid>,
    agent_short_id: Option<String>,
    agent_title: Option<String>,
    agent_tool: Option<String>,
    account_lo: Option<i32>,
    account_hi: Option<i32>,
    to_agent_id: Option<Uuid>,
    to_short_id: Option<String>,
    to_title: Option<String>,
    to_tool: Option<String>,
}

/// Message views by id, in id order. `viewer` decides which side of a direct
/// conversation is the "peer".
pub async fn messages(
    db: &PgPool,
    ids: &[i64],
    viewer: i32,
) -> Result<Vec<MessageView>, sqlx::Error> {
    if ids.is_empty() {
        return Ok(vec![]);
    }
    let rows: Vec<MessageRow> = sqlx::query_as(
        "SELECT m.id, m.body, m.created_at, m.conversation_id, c.kind, c.group_id, g.name AS group_name,
                m.from_account, fa.name AS from_name, m.from_agent_id,
                ag.short_id AS agent_short_id, coalesce(ag.custom_title, ag.title) AS agent_title, ag.tool AS agent_tool,
                c.account_lo, c.account_hi,
                m.to_agent_id, ta.short_id AS to_short_id, coalesce(ta.custom_title, ta.title) AS to_title, ta.tool AS to_tool
         FROM messages m
         JOIN conversations c ON c.id = m.conversation_id
         LEFT JOIN chat_groups g ON g.id = c.group_id
         JOIN accounts fa ON fa.id = m.from_account
         LEFT JOIN agents ag ON ag.id = m.from_agent_id
         LEFT JOIN agents ta ON ta.id = m.to_agent_id
         WHERE m.id = ANY($1)
         ORDER BY m.id",
    )
    .bind(ids)
    .fetch_all(db)
    .await?;

    // Names of the "other side" of direct conversations.
    let mut peer_ids: Vec<i32> = rows
        .iter()
        .filter_map(|r| match (r.account_lo, r.account_hi) {
            (Some(lo), Some(hi)) => Some(if lo == viewer { hi } else { lo }),
            _ => None,
        })
        .collect();
    peer_ids.sort_unstable();
    peer_ids.dedup();
    let names: HashMap<i32, String> = if peer_ids.is_empty() {
        HashMap::new()
    } else {
        sqlx::query_as::<_, (i32, String)>("SELECT id, name FROM accounts WHERE id = ANY($1)")
            .bind(&peer_ids)
            .fetch_all(db)
            .await?
            .into_iter()
            .collect()
    };

    // Attachments, in upload order.
    let file_rows: Vec<(i64, Uuid, String, i64, Vec<u8>, DateTime<Utc>)> = sqlx::query_as(
        "SELECT message_id, id, name, size, sha256, expires_at FROM files
         WHERE message_id = ANY($1) ORDER BY created_at, id",
    )
    .bind(ids)
    .fetch_all(db)
    .await?;
    let mut files: HashMap<i64, Vec<FileView>> = HashMap::new();
    for (message_id, id, name, size, sha256, expires_at) in file_rows {
        files.entry(message_id).or_default().push(FileView {
            id,
            name,
            size: size.max(0) as u64,
            sha256: hex::encode(sha256),
            expires_at,
        });
    }

    Ok(rows
        .into_iter()
        .map(|r| {
            let agent = match (
                r.from_agent_id,
                r.agent_short_id,
                r.agent_title,
                r.agent_tool,
            ) {
                (Some(id), Some(short_id), Some(title), Some(tool)) => Some(AgentRef {
                    id,
                    short_id,
                    title,
                    tool,
                }),
                _ => None,
            };
            let group = match (r.group_id, r.group_name) {
                (Some(id), Some(name)) => Some(GroupRef { id, name }),
                _ => None,
            };
            let peer = match (r.account_lo, r.account_hi) {
                (Some(lo), Some(hi)) => {
                    let id = if lo == viewer { hi } else { lo };
                    names.get(&id).map(|name| AccountRef {
                        id,
                        name: name.clone(),
                    })
                }
                _ => None,
            };
            let reply_to = match (&group, &agent) {
                (Some(g), _) => format!("#{}", g.id),
                (None, Some(a)) => format!("{}/{}", r.from_account, a.short_id),
                (None, None) => r.from_account.to_string(),
            };
            let to = match (r.to_agent_id, r.to_short_id, r.to_title, r.to_tool) {
                (Some(id), Some(short_id), Some(title), Some(tool)) => Some(AgentRef {
                    id,
                    short_id,
                    title,
                    tool,
                }),
                _ => None,
            };
            MessageView {
                from_me: r.from_account == viewer && r.from_agent_id.is_none(),
                state: None,
                id: r.id,
                from: Sender {
                    account_id: r.from_account,
                    name: r.from_name,
                    agent,
                },
                conversation: ConversationRef {
                    kind: r.kind,
                    id: r.conversation_id,
                    group,
                    peer,
                },
                body: r.body,
                created_at: r.created_at,
                reply_to,
                files: files.remove(&r.id).unwrap_or_default(),
                to,
            }
        })
        .collect())
}

pub async fn agent_refs(db: &PgPool, ids: &[Uuid]) -> Result<Vec<AgentRef>, sqlx::Error> {
    let rows: Vec<(Uuid, String, String, String)> = sqlx::query_as(
        "SELECT id, short_id, coalesce(custom_title, title) AS title, tool FROM agents WHERE id = ANY($1) ORDER BY title",
    )
    .bind(ids)
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, short_id, title, tool)| AgentRef {
            id,
            short_id,
            title,
            tool,
        })
        .collect())
}

pub async fn account_ref(db: &PgPool, id: i32) -> Result<Option<AccountRef>, sqlx::Error> {
    // A deleted account keeps its row for others' messages, but is gone (ADR-035).
    let row: Option<(i32, String)> = sqlx::query_as(
        "SELECT id, name FROM accounts
         WHERE id = $1 AND NOT EXISTS (SELECT 1 FROM deleted_accounts WHERE id = $1)",
    )
    .bind(id)
    .fetch_optional(db)
    .await?;
    Ok(row.map(|(id, name)| AccountRef { id, name }))
}
