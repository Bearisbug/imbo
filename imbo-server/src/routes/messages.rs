//! API-015 send, API-016 inbox, API-017 pull, API-024 message,
//! API-043 send to my session, API-044 mark my session's replies read.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::{Extension, Json};
use imbo_proto::api::{
    AccountRef, AgentRef, ConversationRef, GroupRef, InboxItem, InboxResponse, MessageResponse,
    PullResponse, ReadResponse, SendResponse,
};
use imbo_proto::text::{Address, parse_address};
use imbo_proto::{
    Candidate, ErrorCode, MAX_BODY_CHARS, MAX_FILES, MAX_MESSAGE_FILE_BYTES, TOOL_TERMINAL,
};
use serde::Deserialize;
use sqlx::PgPool;
use tracing::info;
use uuid::Uuid;

use super::Body;
use crate::auth::Caller;
use crate::billing;
use crate::delivery;
use crate::error::{ApiError, ApiResult, err};
use crate::safety;
use crate::state::St;
use crate::views;

/// Only `to`, `body`, `client_msg_id`, `files` are read; any sender fields a
/// client adds are ignored (RULE-004).
#[derive(Deserialize)]
pub struct SendIn {
    to: Option<String>,
    body: Option<String>,
    client_msg_id: Option<String>,
    files: Option<Vec<Uuid>>,
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
        "SELECT id, short_id, coalesce(custom_title, title) AS title, tool, online FROM agents WHERE account_id = $1 AND tool <> 'terminal'
         ORDER BY online DESC, last_online_at DESC NULLS LAST",
    )
    .bind(account)
    .fetch_all(db)
    .await
}

/// The account's terminal inbox row (ADR-010), created on first use.
async fn terminal_of(db: &PgPool, account: i32) -> Result<AgentRow, sqlx::Error> {
    sqlx::query_as(
        "INSERT INTO agents (id, account_id, tool, session_id, short_id, title)
         VALUES ($1, $2, $3, $3, $3, $3)
         ON CONFLICT (account_id, tool, session_id) DO UPDATE SET online = false
         RETURNING id, short_id, title, tool, online",
    )
    .bind(Uuid::now_v7())
    .bind(account)
    .bind(TOOL_TERMINAL)
    .fetch_one(db)
    .await
}

/// `who` matched no contact; an existing 8-digit account ID means "not a contact".
async fn not_contact(db: &PgPool, who: &str) -> Result<Option<ApiError>, sqlx::Error> {
    if who.len() != 8 || !who.chars().all(|c| c.is_ascii_digit()) {
        return Ok(None);
    }
    let exists: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM accounts WHERE id = $1)")
        .bind(who.parse::<i32>().unwrap_or_default())
        .fetch_one(db)
        .await?;
    Ok(exists.then(|| {
        err(
            ErrorCode::NotContact,
            format!("{who} is not your contact. Add them first."),
        )
    }))
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
async fn resolve(db: &PgPool, me: &AccountRef, to: &str) -> ApiResult<Target> {
    let Some(addr) = parse_address(to) else {
        return Err(err(
            ErrorCode::RecipientNotFound,
            "Write the recipient as name/session, account-ID/session, a name or account ID alone, a group name or #group-ID.",
        ));
    };
    match addr {
        Address::Group(gid) => {
            let mine = my_groups(db, me.id).await?;
            if let Some(g) = mine.into_iter().find(|g| g.id == gid) {
                return Ok(Target::Group(g));
            }
            let exists: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM chat_groups WHERE id = $1 AND dissolved_at IS NULL)",
            )
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
            let contacts = my_contacts(db, me.id).await?;
            let matched = by_name(&contacts, &who);
            let peer = match matched.as_slice() {
                // My own account ID reaches my own sessions (RULE-011).
                _ if who == me.id.to_string() => me.clone(),
                [one] => (*one).clone(),
                [] => {
                    if let Some(e) = not_contact(db, &who).await? {
                        return Err(e);
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
        // My own account ID alone: my terminal inbox (RULE-011).
        Address::Bare(name) if name == me.id.to_string() => Ok(Target::Direct {
            peer: me.clone(),
            agent: terminal_of(db, me.id).await?,
        }),
        Address::Bare(name) => {
            let groups: Vec<GroupRef> = my_groups(db, me.id)
                .await?
                .into_iter()
                .filter(|g| g.name == name)
                .collect();
            let contacts = my_contacts(db, me.id).await?;
            let people: Vec<AccountRef> = by_name(&contacts, &name).into_iter().cloned().collect();
            match (groups.len(), people.len()) {
                (1, 0) => Ok(Target::Group(groups.into_iter().next().unwrap())),
                (0, 0) => {
                    if let Some(e) = not_contact(db, &name).await? {
                        return Err(e);
                    }
                    Err(err(
                        ErrorCode::RecipientNotFound,
                        format!(
                            "No group or contact called {name}. Contacts take name or name/session; groups take their name or #ID."
                        ),
                    ))
                }
                // Account only: the peer's terminal inbox (ADR-010).
                (0, 1) => {
                    let peer = people.into_iter().next().unwrap();
                    let agent = terminal_of(db, peer.id).await?;
                    Ok(Target::Direct { peer, agent })
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
    let (recipients, terminal): (i64, bool) = sqlx::query_as(
        "SELECT count(*) FILTER (WHERE a.tool <> 'terminal'), coalesce(bool_or(a.tool = 'terminal'), false)
         FROM deliveries d JOIN agents a ON a.id = d.agent_id WHERE d.message_id = $1",
    )
    .bind(id)
    .fetch_one(&st.db)
    .await?;
    let view = views::messages(&st.db, &[id], me).await?.into_iter().next();
    Ok(view.map(|v| SendResponse {
        message_id: id,
        conversation: v.conversation,
        recipients: recipients as u32,
        terminal,
        inactive_members: None,
    }))
}

/// Share locks on these accounts' rows, in ID order.
async fn lock_accounts(tx: &mut sqlx::PgConnection, ids: &[i32]) -> Result<(), sqlx::Error> {
    sqlx::query("SELECT 1 FROM accounts WHERE id = ANY($1) ORDER BY id FOR SHARE")
        .bind(ids)
        .execute(&mut *tx)
        .await?;
    Ok(())
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

fn expired() -> ApiError {
    err(
        ErrorCode::FileExpired,
        "A file was uploaded more than an hour ago and has been cleared. Upload it again.",
    )
}

/// The files must be the sender's own, uploaded within the hour, not sent
/// yet, and 100 MB together (INV-012). Locks them until the send commits.
async fn check_files(tx: &mut sqlx::PgConnection, me: i32, files: &[Uuid]) -> ApiResult<()> {
    let rows: Vec<(i64, String, bool)> = sqlx::query_as(
        "SELECT size, state, created_at < now() - interval '1 hour' FROM files
         WHERE id = ANY($1) AND account_id = $2 FOR UPDATE",
    )
    .bind(files)
    .bind(me)
    .fetch_all(&mut *tx)
    .await?;
    if rows.len() != files.len()
        || rows
            .iter()
            .any(|(_, state, _)| state == "sent" || state == "uploading")
    {
        return Err(err(
            ErrorCode::NotFound,
            "A file isn't one you uploaded, or it was already sent with another message.",
        ));
    }
    if rows.iter().any(|(_, state, old)| state != "ready" || *old) {
        return Err(expired());
    }
    let total: i64 = rows.iter().map(|(size, _, _)| size).sum();
    if total as u64 > MAX_MESSAGE_FILE_BYTES {
        return Err(err(
            ErrorCode::FileTooLarge,
            "The files of one message can be at most 100 MB together.",
        ));
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
    let mut files = req.files.unwrap_or_default();
    files.sort_unstable();
    files.dedup();
    if files.len() > MAX_FILES {
        return Err(err(
            ErrorCode::InvalidRequest,
            "A message can carry at most 20 files.",
        ));
    }
    // With files the body may be empty (DESIGN §9 messages.body).
    if (files.is_empty() && body.trim().is_empty()) || body.chars().count() > MAX_BODY_CHARS {
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
    billing::require_active(&st, c.account_id, "API-015").await?;
    let me = AccountRef {
        id: c.account_id,
        name: c.name.clone(),
    };
    let target = resolve(&st.db, &me, &to).await?;
    if let Target::Direct { agent, .. } = &target
        && c.actor.as_ref().is_some_and(|a| a.id == agent.id)
    {
        return Err(err(
            ErrorCode::InvalidRequest,
            "A session can't message itself.",
        ));
    }
    if let Target::Direct { peer, .. } = &target
        && peer.id != c.account_id
        && !billing::access(&st.db, &st.cfg, peer.id).await?.active()
    {
        info!(
            event = "recipient_inactive",
            from_account = c.account_id,
            to_account = peer.id,
            "send refused"
        );
        return Err(err(
            ErrorCode::RecipientInactive,
            format!(
                "{}'s account is inactive, so they can't receive new messages.",
                peer.name
            ),
        ));
    }
    let done = store(&st, &c, &target, &body, &client_msg_id, &files).await?;
    Ok((StatusCode::CREATED, Json(done)))
}

/// Stores a message from the caller to `target` with its deliveries, the
/// part API-015 and API-043 share.
async fn store(
    st: &St,
    c: &Caller,
    target: &Target,
    body: &str,
    client_msg_id: &str,
    files: &[Uuid],
) -> ApiResult<SendResponse> {
    let from_agent = c.actor.as_ref().map(|a| a.id);

    let mut inactive_members = None;
    let mut tx = st.db.begin().await?;
    // Waits out a deletion, ban or block of the sender's account in
    // progress, then sees it (DESIGN §16).
    let banned: bool =
        sqlx::query_scalar("SELECT banned_at IS NOT NULL FROM accounts WHERE id = $1 FOR SHARE")
            .bind(c.account_id)
            .fetch_one(&mut *tx)
            .await?;
    let deleted: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM deleted_accounts WHERE id = $1)")
            .bind(c.account_id)
            .fetch_one(&mut *tx)
            .await?;
    if deleted {
        return Err(err(ErrorCode::Unauthorized, "This account was deleted."));
    }
    if banned {
        return Err(err(ErrorCode::Unauthorized, "This account was banned."));
    }
    let (conversation, recipients): (ConversationRef, Vec<(Uuid, i32)>) = match target {
        Target::Direct { peer, agent } => {
            // The peer's row before the contact row: deleting an account
            // locks its row, then deletes its contact rows (DESIGN §16).
            sqlx::query("SELECT 1 FROM accounts WHERE id = $1 FOR SHARE")
                .bind(peer.id)
                .execute(&mut *tx)
                .await?;
            let still: Option<i32> = sqlx::query_scalar(
                "SELECT peer_id FROM contacts WHERE account_id = $1 AND peer_id = $2 FOR SHARE",
            )
            .bind(c.account_id)
            .bind(peer.id)
            .fetch_optional(&mut *tx)
            .await?;
            // Writing to myself needs no contact (RULE-011).
            if still.is_none() && peer.id != c.account_id {
                return Err(err(
                    ErrorCode::NotContact,
                    format!("{} is not your contact.", peer.name),
                ));
            }
            // RULE-013, RULE-014: refused and not stored.
            if peer.id != c.account_id {
                safety::check_blocks(&mut tx, c.account_id, peer.id, &peer.name).await?;
                safety::check_not_banned(&mut tx, peer.id, &peer.name).await?;
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
            // RULE-014: a ban of a member in progress commits first and is
            // seen below; none starts until this send commits. Taken before
            // the group lock, because deleting an account locks its row and
            // then its groups' locks (DESIGN §16).
            let members: Vec<i32> = sqlx::query_scalar(
                "SELECT account_id FROM group_members WHERE group_id = $1 AND account_id <> $2",
            )
            .bind(g.id)
            .bind(c.account_id)
            .fetch_all(&mut *tx)
            .await?;
            lock_accounts(&mut tx, &members).await?;
            rate_check(&mut tx, id, c.account_id, true).await?;
            // Under the group lock: removed or dissolved in the meantime (DESIGN §16).
            let (dissolved, member): (bool, bool) = sqlx::query_as(
                "SELECT dissolved_at IS NOT NULL,
                        EXISTS (SELECT 1 FROM group_members WHERE group_id = $1 AND account_id = $2)
                 FROM chat_groups WHERE id = $1",
            )
            .bind(g.id)
            .bind(c.account_id)
            .fetch_one(&mut *tx)
            .await?;
            if dissolved {
                return Err(err(
                    ErrorCode::RecipientNotFound,
                    format!("No group #{}.", g.id),
                ));
            }
            if !member {
                return Err(err(
                    ErrorCode::NotMember,
                    format!("You are not in group #{}.", g.id),
                ));
            }
            let mut rows: Vec<(Uuid, i32)> = sqlx::query_as(
                "SELECT ms.agent_id, ms.account_id FROM member_sessions ms
                 WHERE ms.group_id = $1 AND ($2::uuid IS NULL OR ms.agent_id <> $2)",
            )
            .bind(g.id)
            .bind(from_agent)
            .fetch_all(&mut *tx)
            .await?;
            crate::dev::fault_pause(st, "slow-group-send").await;
            // RULE-008, RULE-014: inactive and banned members are skipped;
            // nobody else active means no send.
            let others: Vec<i32> = sqlx::query_scalar(
                "SELECT account_id FROM group_members WHERE group_id = $1 AND account_id <> $2",
            )
            .bind(g.id)
            .bind(c.account_id)
            .fetch_all(&mut *tx)
            .await?;
            // Those who joined since.
            lock_accounts(&mut tx, &others).await?;
            let inactive = safety::inactive(&st.db, &st.cfg, &others).await?;
            if !others.is_empty() && others.iter().all(|a| inactive.contains(a)) {
                info!(
                    event = "recipient_inactive",
                    from_account = c.account_id,
                    group_id = g.id,
                    "send refused"
                );
                return Err(err(
                    ErrorCode::RecipientInactive,
                    "Everyone else in this group has an inactive account, so nobody can receive it.",
                ));
            }
            // RULE-013: members who block the sender don't get it, and
            // aren't counted. Read under the group lock (DESIGN §16).
            let blockers: Vec<i32> = sqlx::query_scalar(
                "SELECT account_id FROM blocks WHERE blocked_id = $1 AND account_id = ANY($2)",
            )
            .bind(c.account_id)
            .bind(&others)
            .fetch_all(&mut *tx)
            .await?;
            rows.retain(|(_, account)| !inactive.contains(account) && !blockers.contains(account));
            inactive_members = Some(others.iter().filter(|a| inactive.contains(a)).count() as u32);
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
    let to_agent = match target {
        Target::Direct { agent, .. } => Some(agent.id),
        Target::Group(_) => None,
    };
    let terminal = matches!(target, Target::Direct { agent, .. } if agent.tool == TOOL_TERMINAL);
    if !files.is_empty() {
        check_files(&mut tx, c.account_id, files).await?;
    }
    let message_id: Option<i64> = sqlx::query_scalar(
        "INSERT INTO messages (conversation_id, from_account, from_agent_id, to_agent_id, body, client_msg_id)
         VALUES ($1, $2, $3, $4, $5, $6)
         ON CONFLICT (from_account, client_msg_id) DO NOTHING RETURNING id",
    )
    .bind(conversation.id)
    .bind(c.account_id)
    .bind(from_agent)
    .bind(to_agent)
    .bind(body)
    .bind(client_msg_id)
    .fetch_optional(&mut *tx)
    .await?;
    let Some(message_id) = message_id else {
        drop(tx);
        let done = existing_result(st, c.account_id, client_msg_id).await?;
        return done.ok_or_else(|| err(ErrorCode::Unavailable, "Retry."));
    };
    if !files.is_empty() {
        // `state = 'ready'` loses to a cleanup that got there first.
        let r = sqlx::query(
            "UPDATE files SET state = 'sent', message_id = $1 WHERE id = ANY($2) AND state = 'ready'",
        )
        .bind(message_id)
        .bind(files)
        .execute(&mut *tx)
        .await?;
        if r.rows_affected() != files.len() as u64 {
            return Err(expired());
        }
    }
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
    delivery::kick(st, recipients.iter().map(|(a, _)| *a));
    crate::push::messages(st, vec![message_id]);
    Ok(SendResponse {
        message_id,
        conversation,
        recipients: if terminal { 0 } else { recipients.len() as u32 },
        terminal,
        inactive_members,
    })
}

/// One of my sessions as API-014 lists it: not the terminal inbox, seen in
/// the last 7 days.
async fn my_session(st: &St, me: i32, raw: &str) -> ApiResult<AgentRow> {
    let not_found = || err(ErrorCode::NotFound, "No such session.");
    let id = Uuid::parse_str(raw).map_err(|_| not_found())?;
    sqlx::query_as(
        "SELECT id, short_id, coalesce(custom_title, title) AS title, tool, online FROM agents
         WHERE id = $1 AND account_id = $2 AND tool <> 'terminal'
           AND (online OR last_online_at > now() - interval '7 days')",
    )
    .bind(id)
    .bind(me)
    .fetch_optional(&st.db)
    .await?
    .ok_or_else(not_found)
}

/// The delivery state of each message the person sent `agent` (API-036).
pub async fn with_states(
    db: &PgPool,
    messages: &mut [imbo_proto::api::MessageView],
    agent: Uuid,
) -> Result<(), sqlx::Error> {
    let ids: Vec<i64> = messages
        .iter()
        .filter(|m| m.from_me && m.to.as_ref().is_some_and(|t| t.id == agent))
        .map(|m| m.id)
        .collect();
    if ids.is_empty() {
        return Ok(());
    }
    let states: Vec<(i64, String)> = sqlx::query_as(
        "SELECT message_id, state FROM deliveries WHERE agent_id = $1 AND message_id = ANY($2)",
    )
    .bind(agent)
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

#[derive(Deserialize)]
pub struct AgentSendIn {
    body: Option<String>,
    client_msg_id: Option<String>,
    files: Option<Vec<Uuid>>,
}

/// API-043: the web app writes to one of my sessions; the sender is my
/// terminal inbox, as with `imbo send` (ADR-030).
pub async fn send_to_agent(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
    Path(raw): Path<String>,
    Body(req): Body<AgentSendIn>,
) -> ApiResult<(StatusCode, Json<MessageResponse>)> {
    let body = req.body.unwrap_or_default();
    let client_msg_id = req.client_msg_id.unwrap_or_default();
    let mut files = req.files.unwrap_or_default();
    files.sort_unstable();
    files.dedup();
    if files.len() > MAX_FILES {
        return Err(err(
            ErrorCode::InvalidRequest,
            "A message can carry at most 20 files.",
        ));
    }
    if (files.is_empty() && body.trim().is_empty()) || body.chars().count() > MAX_BODY_CHARS {
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
    let agent = my_session(&st, c.account_id, &raw).await?;
    let agent_id = agent.id;
    let message_id = match existing_result(&st, c.account_id, &client_msg_id).await? {
        Some(done) => done.message_id,
        None => {
            billing::require_active(&st, c.account_id, "API-043").await?;
            let target = Target::Direct {
                peer: AccountRef {
                    id: c.account_id,
                    name: c.name.clone(),
                },
                agent,
            };
            let id = store(&st, &c, &target, &body, &client_msg_id, &files)
                .await?
                .message_id;
            if c.web_person() {
                crate::wake::request(&st, c.account_id, agent_id).await?;
            }
            id
        }
    };
    let mut messages = views::messages(&st.db, &[message_id], c.account_id).await?;
    if let Some(to) = messages.first().and_then(|m| m.to.as_ref()).map(|t| t.id) {
        with_states(&st.db, &mut messages, to).await?;
    }
    let message = messages
        .into_iter()
        .next()
        .ok_or_else(|| err(ErrorCode::Unavailable, "Retry."))?;
    Ok((StatusCode::CREATED, Json(MessageResponse { message })))
}

#[derive(Deserialize)]
pub struct ReadIn {
    up_to: Option<i64>,
}

/// API-044: what the web app has shown of a session's replies to my
/// terminal inbox counts as read, as `imbo inbox` would (ADR-030).
pub async fn mark_read(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
    Path(raw): Path<String>,
    Body(req): Body<ReadIn>,
) -> ApiResult<Json<ReadResponse>> {
    let Some(up_to) = req.up_to else {
        return Err(err(ErrorCode::InvalidRequest, "up_to is required."));
    };
    let agent = my_session(&st, c.account_id, &raw).await?;
    let marked = sqlx::query(
        "UPDATE deliveries d SET state = 'delivered', delivered_at = now(), via = 'web'
         FROM messages m, agents t
         WHERE m.id = d.message_id AND t.id = d.agent_id
           AND t.account_id = $1 AND t.tool = 'terminal'
           AND m.from_agent_id = $2 AND m.id <= $3
           AND d.state IN ('pending', 'pushed')",
    )
    .bind(c.account_id)
    .bind(agent.id)
    .bind(up_to)
    .execute(&st.db)
    .await?
    .rows_affected();
    Ok(Json(ReadResponse { marked }))
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
        "SELECT d.message_id, d.agent_id, a.short_id, coalesce(a.custom_title, a.title) AS title, a.tool, d.state FROM deliveries d
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
    let views = views::messages(&st.db, &ids, c.account_id).await?;
    let items = rows
        .into_iter()
        .rev()
        .filter_map(|r| {
            // One message can have several deliveries here (a group message
            // to two of my sessions), so each row gets its own copy.
            let message = views.iter().find(|v| v.id == r.message_id)?.clone();
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
    let via = if q.via.as_deref() == Some("prompt_hook") {
        "prompt_hook"
    } else {
        "pull"
    };
    // An agent takes its own unread; the person takes the terminal inbox.
    let ids: Vec<i64> = sqlx::query_scalar(
        "UPDATE deliveries d SET state = 'delivered', delivered_at = now(), via = $3
         FROM agents a
         WHERE a.id = d.agent_id AND d.state IN ('pending', 'pushed')
           AND (a.id = $1 OR ($1 IS NULL AND a.account_id = $2 AND a.tool = 'terminal'))
         RETURNING d.message_id",
    )
    .bind(c.actor.as_ref().map(|a| a.id))
    .bind(c.account_id)
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
