//! API-015 send, API-016 inbox, API-017 pull, API-024 message.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::{Extension, Json};
use imbo_proto::api::{
    AccountRef, AgentRef, ConversationRef, GroupRef, InboxItem, InboxResponse, MessageResponse,
    PullResponse, SendResponse,
};
use imbo_proto::text::{Address, parse_address};
use imbo_proto::{Candidate, ErrorCode, MAX_BODY_CHARS};
use serde::Deserialize;
use sqlx::PgPool;
use uuid::Uuid;

use super::Body;
use crate::auth::Caller;
use crate::delivery;
use crate::error::{ApiError, ApiResult, err};
use crate::state::St;
use crate::views;

/// Only `to`, `body`, `client_msg_id` are read; any sender fields a client
/// adds are ignored (RULE-004).
#[derive(Deserialize)]
pub struct SendIn {
    to: Option<String>,
    body: Option<String>,
    client_msg_id: Option<String>,
}

#[derive(Clone, sqlx::FromRow)]
struct AgentRow {
    id: Uuid,
    short_id: String,
    title: String,
    tool: String,
    online: bool,
}

enum Target {
    Direct { peer: AccountRef, agent: AgentRow },
    Group(GroupRef),
}

fn session_candidates(peer: &AccountRef, agents: &[AgentRow]) -> Vec<Candidate> {
    agents
        .iter()
        .filter(|a| a.online)
        .map(|a| Candidate {
            to: format!("{}/{}", peer.id, a.short_id),
            label: format!("{} · {} ({}, {})", peer.name, a.title, a.tool, a.short_id),
            online: Some(true),
        })
        .collect()
}

fn contact_candidate(p: &AccountRef) -> Candidate {
    Candidate {
        to: format!("{}/<session>", p.id),
        label: format!("{} ({})", p.name, p.id),
        online: None,
    }
}

fn group_candidate(g: &GroupRef) -> Candidate {
    Candidate {
        to: format!("#{}", g.id),
        label: format!("group {} (#{})", g.name, g.id),
        online: None,
    }
}

async fn my_contacts(db: &PgPool, me: i32) -> Result<Vec<AccountRef>, sqlx::Error> {
    let rows: Vec<(i32, String)> = sqlx::query_as(
        "SELECT a.id, a.name FROM contacts c JOIN accounts a ON a.id = c.peer_id WHERE c.account_id = $1 ORDER BY a.id",
    )
    .bind(me)
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, name)| AccountRef { id, name })
        .collect())
}

async fn my_groups(db: &PgPool, me: i32) -> Result<Vec<GroupRef>, sqlx::Error> {
    let rows: Vec<(i32, String)> = sqlx::query_as(
        "SELECT g.id, g.name FROM group_members m JOIN chat_groups g ON g.id = m.group_id WHERE m.account_id = $1 ORDER BY g.id",
    )
    .bind(me)
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, name)| GroupRef { id, name })
        .collect())
}

async fn agents_of(db: &PgPool, account: i32) -> Result<Vec<AgentRow>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, short_id, title, tool, online FROM agents WHERE account_id = $1
         ORDER BY online DESC, last_online_at DESC NULLS LAST",
    )
    .bind(account)
    .fetch_all(db)
    .await
}

fn by_name<'a>(contacts: &'a [AccountRef], who: &str) -> Vec<&'a AccountRef> {
    if let Ok(id) = who.parse::<i32>()
        && who.len() == 8
    {
        return contacts.iter().filter(|c| c.id == id).collect();
    }
    let exact: Vec<_> = contacts.iter().filter(|c| c.name == who).collect();
    if !exact.is_empty() {
        return exact;
    }
    contacts
        .iter()
        .filter(|c| c.name.to_lowercase() == who.to_lowercase())
        .collect()
}

/// DESIGN §9 收件人写法.
async fn resolve(db: &PgPool, me: i32, to: &str) -> ApiResult<Target> {
    let Some(addr) = parse_address(to) else {
        return Err(err(
            ErrorCode::RecipientNotFound,
            "Write the recipient as name/session, account-ID/session, a group name or #group-ID.",
        ));
    };
    match addr {
        Address::Group(gid) => {
            let mine = my_groups(db, me).await?;
            if let Some(g) = mine.into_iter().find(|g| g.id == gid) {
                return Ok(Target::Group(g));
            }
            let exists: bool =
                sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM chat_groups WHERE id = $1)")
                    .bind(gid)
                    .fetch_one(db)
                    .await?;
            if exists {
                Err(err(
                    ErrorCode::NotMember,
                    format!("You are not in group #{gid}."),
                ))
            } else {
                Err(err(
                    ErrorCode::RecipientNotFound,
                    format!("No group #{gid}."),
                ))
            }
        }
        Address::Direct { who, session } => {
            let contacts = my_contacts(db, me).await?;
            let matched = by_name(&contacts, &who);
            let peer = match matched.as_slice() {
                [one] => (*one).clone(),
                [] => {
                    if who.len() == 8 && who.chars().all(|c| c.is_ascii_digit()) {
                        let exists: bool = sqlx::query_scalar(
                            "SELECT EXISTS (SELECT 1 FROM accounts WHERE id = $1)",
                        )
                        .bind(who.parse::<i32>().unwrap_or_default())
                        .fetch_one(db)
                        .await?;
                        if exists {
                            return Err(err(
                                ErrorCode::NotContact,
                                format!("{who} is not your contact. Add them first."),
                            ));
                        }
                    }
                    return Err(err(
                        ErrorCode::RecipientNotFound,
                        format!("No contact named {who}."),
                    )
                    .candidates(contacts.iter().map(contact_candidate).collect()));
                }
                many => {
                    return Err(err(
                        ErrorCode::AmbiguousRecipient,
                        format!(
                            "{} contacts are named {who}. Use the account ID instead.",
                            many.len()
                        ),
                    )
                    .candidates(many.iter().map(|p| contact_candidate(p)).collect()));
                }
            };
            let agents = agents_of(db, peer.id).await?;
            let s = session.to_lowercase();
            if let Some(a) = agents.iter().find(|a| a.short_id == s) {
                return Ok(Target::Direct {
                    peer,
                    agent: a.clone(),
                });
            }
            let mut titled: Vec<&AgentRow> = agents.iter().filter(|a| a.title == session).collect();
            // A title shared with offline sessions still means the online one.
            if titled.len() > 1 && titled.iter().filter(|a| a.online).count() == 1 {
                titled.retain(|a| a.online);
            }
            match titled.as_slice() {
                [one] => Ok(Target::Direct {
                    peer,
                    agent: (*one).clone(),
                }),
                [] => Err(err(
                    ErrorCode::SessionNotFound,
                    format!("{} has no session called \"{session}\".", peer.name),
                )
                .candidates(session_candidates(&peer, &agents))),
                many => {
                    let rows: Vec<AgentRow> = many.iter().map(|a| (*a).clone()).collect();
                    let mut c = session_candidates(&peer, &rows);
                    if c.is_empty() {
                        c = rows
                            .iter()
                            .map(|a| Candidate {
                                to: format!("{}/{}", peer.id, a.short_id),
                                label: format!("{} · {} ({})", peer.name, a.title, a.short_id),
                                online: Some(a.online),
                            })
                            .collect();
                    }
                    Err(err(
                        ErrorCode::AmbiguousRecipient,
                        format!(
                            "{} has {} sessions titled \"{session}\". Use the short ID.",
                            peer.name,
                            many.len()
                        ),
                    )
                    .candidates(c))
                }
            }
        }
        Address::Bare(name) => {
            let groups: Vec<GroupRef> = my_groups(db, me)
                .await?
                .into_iter()
                .filter(|g| g.name == name)
                .collect();
            let contacts = my_contacts(db, me).await?;
            let people: Vec<AccountRef> = by_name(&contacts, &name).into_iter().cloned().collect();
            match (groups.len(), people.len()) {
                (1, 0) => Ok(Target::Group(groups.into_iter().next().unwrap())),
                (0, 0) => Err(err(
                    ErrorCode::RecipientNotFound,
                    format!(
                        "No group or contact called {name}. Contacts take name/session; groups take their name or #ID."
                    ),
                )),
                (0, 1) => {
                    let peer = people.into_iter().next().unwrap();
                    let agents = agents_of(db, peer.id).await?;
                    Err(err(
                        ErrorCode::SessionRequired,
                        format!(
                            "Say which of {}'s sessions should get it: {}/<session>.",
                            peer.name, peer.name
                        ),
                    )
                    .candidates(session_candidates(&peer, &agents)))
                }
                _ => {
                    let mut c: Vec<Candidate> = groups.iter().map(group_candidate).collect();
                    c.extend(people.iter().map(contact_candidate));
                    Err(err(
                        ErrorCode::AmbiguousRecipient,
                        format!("\"{name}\" matches more than one group or contact."),
                    )
                    .candidates(c))
                }
            }
        }
    }
}

async fn existing_result(st: &St, me: i32, client_msg_id: &str) -> ApiResult<Option<SendResponse>> {
    let row: Option<(i64, i64)> = sqlx::query_as(
        "SELECT id, conversation_id FROM messages WHERE from_account = $1 AND client_msg_id = $2",
    )
    .bind(me)
    .bind(client_msg_id)
    .fetch_optional(&st.db)
    .await?;
    let Some((id, _)) = row else { return Ok(None) };
    let recipients: i64 =
        sqlx::query_scalar("SELECT count(*) FROM deliveries WHERE message_id = $1")
            .bind(id)
            .fetch_one(&st.db)
            .await?;
    let view = views::messages(&st.db, &[id], me).await?.into_iter().next();
    Ok(view.map(|v| SendResponse {
        message_id: id,
        conversation: v.conversation,
        recipients: recipients as u32,
    }))
}

async fn rate_check(
    tx: &mut sqlx::PgConnection,
    conversation: i64,
    me: i32,
    group: bool,
) -> ApiResult<()> {
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(-conversation)
        .execute(&mut *tx)
        .await?;
    let (n, oldest): (i64, Option<f64>) = sqlx::query_as(
        "SELECT count(*), extract(epoch FROM now() - min(created_at))::float8 FROM messages
         WHERE conversation_id = $1 AND created_at > now() - interval '60 seconds'",
    )
    .bind(conversation)
    .fetch_one(&mut *tx)
    .await?;
    if n >= 30 {
        let wait = (60.0 - oldest.unwrap_or(0.0)).ceil().max(1.0) as u64;
        let what = if group {
            "This group already had 30 messages in the last minute."
        } else {
            "This conversation already had 30 messages in the last minute."
        };
        return Err(err(
            ErrorCode::RateLimited,
            format!("{what} Try again in {wait} s."),
        )
        .retry_after(wait));
    }
    let (n, oldest): (i64, Option<f64>) = sqlx::query_as(
        "SELECT count(*), extract(epoch FROM now() - min(created_at))::float8 FROM messages
         WHERE from_account = $1 AND created_at > now() - interval '60 seconds'",
    )
    .bind(me)
    .fetch_one(&mut *tx)
    .await?;
    if n >= 120 {
        let wait = (60.0 - oldest.unwrap_or(0.0)).ceil().max(1.0) as u64;
        return Err(err(
            ErrorCode::RateLimited,
            format!("You sent 120 messages in the last minute. Try again in {wait} s."),
        )
        .retry_after(wait));
    }
    Ok(())
}

pub async fn send(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
    Body(req): Body<SendIn>,
) -> ApiResult<(StatusCode, Json<SendResponse>)> {
    let to = req.to.unwrap_or_default();
    let body = req.body.unwrap_or_default();
    let client_msg_id = req.client_msg_id.unwrap_or_default();
    if body.trim().is_empty() || body.chars().count() > MAX_BODY_CHARS {
        return Err(err(
            ErrorCode::BodyInvalid,
            "The message must be 1 to 32000 characters.",
        ));
    }
    if client_msg_id.is_empty() || client_msg_id.len() > 64 {
        return Err(err(
            ErrorCode::InvalidRequest,
            "client_msg_id must be 1 to 64 characters.",
        ));
    }
    if let Some(done) = existing_result(&st, c.account_id, &client_msg_id).await? {
        return Ok((StatusCode::CREATED, Json(done)));
    }
    let target = resolve(&st.db, c.account_id, &to).await?;
    let from_agent = c.actor.as_ref().map(|a| a.id);

    let mut tx = st.db.begin().await?;
    let (conversation, recipients): (ConversationRef, Vec<(Uuid, i32)>) = match &target {
        Target::Direct { peer, agent } => {
            let still: Option<i32> = sqlx::query_scalar(
                "SELECT peer_id FROM contacts WHERE account_id = $1 AND peer_id = $2 FOR SHARE",
            )
            .bind(c.account_id)
            .bind(peer.id)
            .fetch_optional(&mut *tx)
            .await?;
            if still.is_none() {
                return Err(err(
                    ErrorCode::NotContact,
                    format!("{} is not your contact.", peer.name),
                ));
            }
            let (lo, hi) = if c.account_id < peer.id {
                (c.account_id, peer.id)
            } else {
                (peer.id, c.account_id)
            };
            sqlx::query(
                "INSERT INTO conversations (kind, account_lo, account_hi) VALUES ('direct', $1, $2) ON CONFLICT DO NOTHING",
            )
            .bind(lo)
            .bind(hi)
            .execute(&mut *tx)
            .await?;
            let id: i64 = sqlx::query_scalar(
                "SELECT id FROM conversations WHERE account_lo = $1 AND account_hi = $2",
            )
            .bind(lo)
            .bind(hi)
            .fetch_one(&mut *tx)
            .await?;
            rate_check(&mut tx, id, c.account_id, false).await?;
            (
                ConversationRef {
                    kind: "direct".into(),
                    id,
                    group: None,
                    peer: Some(peer.clone()),
                },
                vec![(agent.id, peer.id)],
            )
        }
        Target::Group(g) => {
            let id: i64 = sqlx::query_scalar("SELECT id FROM conversations WHERE group_id = $1")
                .bind(g.id)
                .fetch_one(&mut *tx)
                .await?;
            rate_check(&mut tx, id, c.account_id, true).await?;
            let rows: Vec<(Uuid, i32)> = sqlx::query_as(
                "SELECT ms.agent_id, ms.account_id FROM member_sessions ms
                 WHERE ms.group_id = $1 AND ($2::uuid IS NULL OR ms.agent_id <> $2)",
            )
            .bind(g.id)
            .bind(from_agent)
            .fetch_all(&mut *tx)
            .await?;
            (
                ConversationRef {
                    kind: "group".into(),
                    id,
                    group: Some(g.clone()),
                    peer: None,
                },
                rows,
            )
        }
    };
    let to_agent = match &target {
        Target::Direct { agent, .. } => Some(agent.id),
        Target::Group(_) => None,
    };
    let message_id: Option<i64> = sqlx::query_scalar(
        "INSERT INTO messages (conversation_id, from_account, from_agent_id, to_agent_id, body, client_msg_id)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (from_account, client_msg_id) DO NOTHING RETURNING id",
    )
    .bind(conversation.id)
    .bind(c.account_id)
    .bind(from_agent)
    .bind(to_agent)
    .bind(&body)
    .bind(&client_msg_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(message_id) = message_id else {
        drop(tx);
        let done = existing_result(&st, c.account_id, &client_msg_id).await?;
        return done
            .map(|d| (StatusCode::CREATED, Json(d)))
            .ok_or_else(|| err(ErrorCode::Unavailable, "Retry."));
    };
    for (agent, account) in &recipients {
        sqlx::query(
            "INSERT INTO deliveries (message_id, agent_id, account_id) VALUES ($1, $2, $3)",
        )
        .bind(message_id)
        .bind(agent)
        .bind(account)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await?;
    delivery::kick(&st, recipients.iter().map(|(a, _)| *a));
    Ok((
        StatusCode::CREATED,
        Json(SendResponse {
            message_id,
            conversation,
            recipients: recipients.len() as u32,
        }),
    ))
}

#[derive(Deserialize)]
pub struct InboxQuery {
    agent: Option<Uuid>,
    all: Option<bool>,
}

#[derive(sqlx::FromRow)]
struct InboxRow {
    message_id: i64,
    agent_id: Uuid,
    short_id: String,
    title: String,
    tool: String,
    state: String,
}

pub async fn list_inbox(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
    Query(q): Query<InboxQuery>,
) -> ApiResult<Json<InboxResponse>> {
    let agent = q.agent.or_else(|| c.actor.as_ref().map(|a| a.id));
    let rows: Vec<InboxRow> = sqlx::query_as(
        "SELECT d.message_id, d.agent_id, a.short_id, a.title, a.tool, d.state FROM deliveries d
         JOIN agents a ON a.id = d.agent_id
         WHERE d.account_id = $1 AND ($2::uuid IS NULL OR d.agent_id = $2)
           AND (d.state IN ('pending', 'pushed') OR ($3 AND d.state = 'delivered'))
         ORDER BY d.message_id DESC LIMIT 100",
    )
    .bind(c.account_id)
    .bind(agent)
    .bind(q.all.unwrap_or(false))
    .fetch_all(&st.db)
    .await?;
    let ids: Vec<i64> = rows.iter().map(|r| r.message_id).collect();
    let mut views = views::messages(&st.db, &ids, c.account_id).await?;
    let items = rows
        .into_iter()
        .rev()
        .filter_map(|r| {
            let pos = views.iter().position(|v| v.id == r.message_id)?;
            let message = views.swap_remove(pos);
            Some(InboxItem {
                message,
                agent: AgentRef {
                    id: r.agent_id,
                    short_id: r.short_id,
                    title: r.title,
                    tool: r.tool,
                },
                state: r.state,
            })
        })
        .collect();
    Ok(Json(InboxResponse { items }))
}

#[derive(Deserialize)]
pub struct PullQuery {
    via: Option<String>,
}

pub async fn pull(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
    Query(q): Query<PullQuery>,
) -> ApiResult<Json<PullResponse>> {
    let Some(actor) = &c.actor else {
        return Err(err(
            ErrorCode::Forbidden,
            "Only an agent session can pull its unread messages.",
        ));
    };
    let via = if q.via.as_deref() == Some("prompt_hook") {
        "prompt_hook"
    } else {
        "pull"
    };
    let ids: Vec<i64> = sqlx::query_scalar(
        "UPDATE deliveries SET state = 'delivered', delivered_at = now(), via = $2
         WHERE agent_id = $1 AND state IN ('pending', 'pushed') RETURNING message_id",
    )
    .bind(actor.id)
    .bind(via)
    .fetch_all(&st.db)
    .await?;
    let mut ids = ids;
    ids.sort_unstable();
    let messages = views::messages(&st.db, &ids, c.account_id).await?;
    Ok(Json(PullResponse { messages }))
}

pub async fn get_message(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
    Path(id): Path<i64>,
) -> ApiResult<Json<MessageResponse>> {
    let visible: bool = sqlx::query_scalar(
        "SELECT EXISTS (
           SELECT 1 FROM messages m JOIN conversations cv ON cv.id = m.conversation_id
           WHERE m.id = $1 AND (
             m.from_account = $2 OR cv.account_lo = $2 OR cv.account_hi = $2
             OR EXISTS (SELECT 1 FROM group_members gm WHERE gm.group_id = cv.group_id AND gm.account_id = $2)
             OR EXISTS (SELECT 1 FROM deliveries d WHERE d.message_id = m.id AND d.account_id = $2)))",
    )
    .bind(id)
    .bind(c.account_id)
    .fetch_one(&st.db)
    .await?;
    let not_found = || -> ApiError { err(ErrorCode::NotFound, format!("No message {id}.")) };
    if !visible {
        return Err(not_found());
    }
    let message = views::messages(&st.db, &[id], c.account_id)
        .await?
        .into_iter()
        .next()
        .ok_or_else(not_found)?;
    Ok(Json(MessageResponse { message }))
}
