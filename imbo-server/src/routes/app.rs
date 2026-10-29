//! API-049 appLogout, API-050 registerPush, API-051 unregisterPush,
//! API-052 deleteAccount.

use axum::Extension;
use axum::extract::State;
use axum::http::StatusCode;
use imbo_proto::ErrorCode;
use imbo_proto::api::{DeleteAccountRequest, PushTokenRequest};
use imbo_proto::frames::CLOSE_UNAUTHORIZED;
use tracing::info;
use uuid::Uuid;

use super::Body;
use super::groups::{cancel_group, lock_group, post_notice};
use crate::auth::Caller;
use crate::error::{ApiResult, err};
use crate::state::St;
use crate::{apple, avatar, delivery, push, safety};

const MAX_PUSH_TOKEN: usize = 200;

/// API-049–051 change only the App sign-in the request came with.
fn app_only(c: &Caller) -> ApiResult<Uuid> {
    c.app
        .ok_or_else(|| err(ErrorCode::Forbidden, "Only the Imbo App can do this."))
}

/// API-049: end this App sign-in, with its push token.
pub async fn logout(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
) -> ApiResult<StatusCode> {
    let app = app_only(&c)?;
    sqlx::query(
        "UPDATE app_sessions SET revoked_at = now(), push_token = NULL
         WHERE id = $1 AND revoked_at IS NULL",
    )
    .bind(app)
    .execute(&st.db)
    .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// API-050: notify this phone from now on. A token one phone hands out
/// belongs to the latest sign-in that registered it (INV-026).
pub async fn register_push(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
    Body(req): Body<PushTokenRequest>,
) -> ApiResult<StatusCode> {
    let app = app_only(&c)?;
    let token = req.push_token.trim();
    if !(token.starts_with("ExponentPushToken[") && token.ends_with(']'))
        || token.len() > MAX_PUSH_TOKEN
    {
        return Err(err(
            ErrorCode::InvalidRequest,
            "push_token must be an Expo push token, ExponentPushToken[…].",
        ));
    }
    let mut tx = st.db.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(6, hashtext($1))")
        .bind(token)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE app_sessions SET push_token = NULL WHERE push_token = $1 AND id <> $2")
        .bind(token)
        .bind(app)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE app_sessions SET push_token = $1 WHERE id = $2")
        .bind(token)
        .bind(app)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// API-051: stop notifying this phone.
pub async fn unregister_push(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
) -> ApiResult<StatusCode> {
    let app = app_only(&c)?;
    sqlx::query("UPDATE app_sessions SET push_token = NULL WHERE id = $1")
        .bind(app)
        .execute(&st.db)
        .await?;
    Ok(StatusCode::NO_CONTENT)
}

/// API-052: delete the caller's account now (ADR-035). Others keep the
/// messages they sent; the account row stays, scrubbed, for those to point
/// at, and its ID is never handed out again.
pub async fn delete_account(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
    Body(req): Body<DeleteAccountRequest>,
) -> ApiResult<StatusCode> {
    if req.confirm != "DELETE" {
        return Err(err(ErrorCode::InvalidRequest, "Type DELETE to confirm."));
    }
    apple::check_before_delete(&st, c.account_id).await?;
    delete_now(&st, c.account_id, "its owner deleted their account").await?;
    Ok(StatusCode::NO_CONTENT)
}

/// The deletion API-052 and API-070 share: subscriptions canceled first,
/// then everything in one transaction. Groups it owns are dissolved with a
/// notice ending "because <why>."
pub(crate) async fn delete_now(st: &St, me: i32, why: &str) -> ApiResult<()> {
    // Stop charging first; if Waffo can't, nothing is deleted.
    safety::cancel_subscriptions(&st.db, &st.cfg, &st.http, me).await?;

    let mut tx = st.db.begin().await?;
    // Blocks this account's own sends, which take FOR SHARE (DESIGN §16).
    sqlx::query("SELECT 1 FROM accounts WHERE id = $1 FOR NO KEY UPDATE")
        .bind(me)
        .execute(&mut *tx)
        .await?;
    // Requests before contacts, so a request accepted meanwhile is either
    // refused or its contacts are deleted below.
    sqlx::query("DELETE FROM requests WHERE from_account = $1 OR to_account = $1")
        .bind(me)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM contacts WHERE account_id = $1 OR peer_id = $1")
        .bind(me)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM blocks WHERE account_id = $1 OR blocked_id = $1")
        .bind(me)
        .execute(&mut *tx)
        .await?;

    // Groups, under each group's lock in ID order; owned ones are dissolved
    // as API-039 does.
    let groups: Vec<(i32, String, String)> = sqlx::query_as(
        "SELECT g.id, g.name, m.role FROM group_members m JOIN chat_groups g ON g.id = m.group_id
         WHERE m.account_id = $1 ORDER BY g.id",
    )
    .bind(me)
    .fetch_all(&mut *tx)
    .await?;
    let mut canceled = vec![];
    let mut dissolved = vec![];
    let mut avatars: Vec<Uuid> = sqlx::query_scalar(
        "SELECT avatar_id FROM accounts WHERE id = $1 AND avatar_id IS NOT NULL",
    )
    .bind(me)
    .fetch_all(&mut *tx)
    .await?;
    for (gid, name, role) in &groups {
        lock_group(&mut tx, *gid).await?;
        if role != "owner" {
            continue;
        }
        let members: Vec<i32> = sqlx::query_scalar(
            "DELETE FROM group_members WHERE group_id = $1 AND account_id <> $2 RETURNING account_id",
        )
        .bind(gid)
        .bind(me)
        .fetch_all(&mut *tx)
        .await?;
        let picture: Option<Uuid> = sqlx::query_scalar(
            "UPDATE chat_groups SET dissolved_at = now() WHERE id = $1 RETURNING avatar_id",
        )
        .bind(gid)
        .fetch_one(&mut *tx)
        .await?;
        avatars.extend(picture);
        canceled.extend(cancel_group(&mut tx, *gid, None, "group_dissolved").await?);
        dissolved.push((*gid, name.clone(), members));
    }
    sqlx::query("DELETE FROM group_members WHERE account_id = $1")
        .bind(me)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM group_removals WHERE account_id = $1")
        .bind(me)
        .execute(&mut *tx)
        .await?;

    // His messages go; those already pushed are taken back with `cancel`.
    let gone: Vec<(i64, Uuid, String)> = sqlx::query_as(
        "DELETE FROM deliveries d USING messages m
         WHERE d.message_id = m.id AND m.from_account = $1
         RETURNING d.message_id, d.agent_id, d.state",
    )
    .bind(me)
    .fetch_all(&mut *tx)
    .await?;
    canceled.extend(
        gone.into_iter()
            .filter(|(_, _, state)| state == "pushed")
            .map(|(m, a, _)| (m, a)),
    );
    sqlx::query("DELETE FROM deliveries WHERE account_id = $1")
        .bind(me)
        .execute(&mut *tx)
        .await?;
    let files: Vec<Uuid> =
        sqlx::query_scalar("DELETE FROM files WHERE account_id = $1 RETURNING id")
            .bind(me)
            .fetch_all(&mut *tx)
            .await?;
    // Others' messages to one of his sessions stay, without the session.
    sqlx::query(
        "UPDATE messages SET to_agent_id = NULL
         WHERE from_account <> $1 AND to_agent_id IN (SELECT id FROM agents WHERE account_id = $1)",
    )
    .bind(me)
    .execute(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM messages WHERE from_account = $1")
        .bind(me)
        .execute(&mut *tx)
        .await?;
    // Direct conversations nothing is left in, his own included.
    sqlx::query(
        "DELETE FROM conversations c
         WHERE c.kind = 'direct' AND $1 IN (c.account_lo, c.account_hi)
           AND NOT EXISTS (SELECT 1 FROM messages m WHERE m.conversation_id = c.id)",
    )
    .bind(me)
    .execute(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM agents WHERE account_id = $1")
        .bind(me)
        .execute(&mut *tx)
        .await?;
    let devices: Vec<Uuid> =
        sqlx::query_scalar("DELETE FROM devices WHERE account_id = $1 RETURNING id")
            .bind(me)
            .fetch_all(&mut *tx)
            .await?;
    sqlx::query("DELETE FROM web_sessions WHERE account_id = $1")
        .bind(me)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM app_sessions WHERE account_id = $1")
        .bind(me)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "DELETE FROM email_codes
         WHERE account_id = $1 OR lower(email) = (SELECT lower(email) FROM accounts WHERE id = $1)",
    )
    .bind(me)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE accounts SET email = 'deleted-' || id || '@deleted.invalid', name = 'Deleted account'
         WHERE id = $1",
    )
    .bind(me)
    .execute(&mut *tx)
    .await?;
    sqlx::query("INSERT INTO deleted_accounts (id) VALUES ($1)")
        .bind(me)
        .execute(&mut *tx)
        .await?;
    avatar::delete(&mut tx, &avatars).await?;

    // RULE-008, RULE-014: inactive members get no notice (INV-014).
    let mut notices = vec![];
    for (gid, name, members) in &dissolved {
        let inactive = safety::inactive(&st.db, &st.cfg, members).await?;
        for to in members {
            if inactive.contains(to) {
                continue;
            }
            let body = format!("Group {name} (#{gid}) was dissolved because {why}.");
            notices.push(post_notice(&mut tx, me, *to, body).await?);
        }
    }
    tx.commit().await?;

    for d in devices {
        st.hub.close(d, CLOSE_UNAUTHORIZED, "account deleted");
    }
    delivery::notify_canceled(st, canceled).await;
    let (inbox, messages): (Vec<Uuid>, Vec<i64>) = notices.into_iter().unzip();
    delivery::kick(st, inbox);
    push::messages(st, messages);
    let dir = st.files_dir();
    for id in &files {
        let _ = tokio::fs::remove_file(dir.join(id.to_string())).await;
        let _ = tokio::fs::remove_file(dir.join(format!("{id}.part"))).await;
    }
    info!(
        event = "account_deleted",
        account_id = me,
        groups_dissolved = dissolved.len(),
        "account deleted"
    );
    Ok(())
}
