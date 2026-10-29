//! Blocks and bans as other endpoints see them (RULE-013, RULE-014), and
//! the ban itself, shared by API-064, API-066 and `imbo-server account ban`.

use std::collections::{HashMap, HashSet};

use imbo_proto::ErrorCode;
use sqlx::{PgConnection, PgPool};
use tracing::{info, warn};
use uuid::Uuid;

use crate::config::Config;
use crate::error::{ApiResult, err};
use crate::mail::Mailer;
use crate::{apple, billing};

/// Postgres channel a ban done outside the server is announced on, so the
/// server can close the banned account's connections (API-066).
pub const BAN_CHANNEL: &str = "imbo_account_banned";

/// Accounts that can't receive new messages: banned (RULE-014), or with
/// billing on, trial over and no subscription (RULE-008).
pub async fn inactive(db: &PgPool, cfg: &Config, ids: &[i32]) -> Result<HashSet<i32>, sqlx::Error> {
    let mut out: HashSet<i32> =
        sqlx::query_scalar("SELECT id FROM accounts WHERE id = ANY($1) AND banned_at IS NOT NULL")
            .bind(ids)
            .fetch_all(db)
            .await?
            .into_iter()
            .collect();
    if cfg.billing_on() {
        let access = billing::access_many(db, cfg, ids).await?;
        out.extend(
            access
                .into_iter()
                .filter(|(_, a)| !a.active())
                .map(|(id, _)| id),
        );
    }
    Ok(out)
}

/// Avatar addresses of these accounts; banned ones show none (RULE-014).
pub async fn avatars(
    db: &PgPool,
    cfg: &Config,
    ids: &[i32],
) -> Result<HashMap<i32, String>, sqlx::Error> {
    let rows: Vec<(i32, Uuid)> = sqlx::query_as(
        "SELECT id, avatar_id FROM accounts
         WHERE id = ANY($1) AND avatar_id IS NOT NULL AND banned_at IS NULL",
    )
    .bind(ids)
    .fetch_all(db)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, a)| (id, crate::avatar::url(cfg, a)))
        .collect())
}

/// Whom `me` blocks, among `ids`.
pub async fn blocked_by(db: &PgPool, me: i32, ids: &[i32]) -> Result<HashSet<i32>, sqlx::Error> {
    Ok(sqlx::query_scalar(
        "SELECT blocked_id FROM blocks WHERE account_id = $1 AND blocked_id = ANY($2)",
    )
    .bind(me)
    .bind(ids)
    .fetch_all(db)
    .await?
    .into_iter()
    .collect())
}

/// RULE-013 for a direct message, contact request or invite from `me` to
/// `peer`: refused when either blocks the other.
pub async fn check_blocks(
    conn: &mut PgConnection,
    me: i32,
    peer: i32,
    peer_name: &str,
) -> ApiResult<()> {
    let (mine, theirs): (bool, bool) = sqlx::query_as(
        "SELECT EXISTS (SELECT 1 FROM blocks WHERE account_id = $1 AND blocked_id = $2),
                EXISTS (SELECT 1 FROM blocks WHERE account_id = $2 AND blocked_id = $1)",
    )
    .bind(me)
    .bind(peer)
    .fetch_one(&mut *conn)
    .await?;
    if mine {
        return Err(err(
            ErrorCode::UnblockFirst,
            format!("You blocked {peer_name}. Unblock them first."),
        ));
    }
    if theirs {
        info!(
            event = "blocked",
            from_account = me,
            to_account = peer,
            "refused"
        );
        return Err(err(
            ErrorCode::Blocked,
            format!("{peer_name} doesn't accept your messages."),
        ));
    }
    Ok(())
}

/// RULE-014: a banned recipient. Takes a share lock on its row so a ban in
/// progress finishes first (DESIGN §16).
pub async fn check_not_banned(
    conn: &mut PgConnection,
    peer: i32,
    peer_name: &str,
) -> ApiResult<()> {
    let banned: bool =
        sqlx::query_scalar("SELECT banned_at IS NOT NULL FROM accounts WHERE id = $1 FOR SHARE")
            .bind(peer)
            .fetch_optional(&mut *conn)
            .await?
            .unwrap_or(false);
    if banned {
        return Err(err(
            ErrorCode::RecipientInactive,
            format!("{peer_name}'s account is inactive, so they can't receive new messages."),
        ));
    }
    Ok(())
}

/// What a ban's transaction changed, for the caller to act on after commit.
#[derive(Default)]
pub struct Banned {
    /// False when the account was already banned and nothing else changed.
    pub newly: bool,
    pub devices: Vec<Uuid>,
    pub canceled: Vec<(i64, Uuid)>,
}

/// The writes of a ban, in the caller's transaction: lock the account row,
/// record the time, end every sign-in, take back undelivered messages to
/// and from it, close pending requests both ways and the other open reports
/// on it (API-066, ENT-Account normal → banned). `by` is the admin, `None`
/// from the server command.
pub async fn ban_tx(tx: &mut PgConnection, target: i32, by: Option<i32>) -> ApiResult<Banned> {
    let row: Option<bool> = sqlx::query_scalar(
        "SELECT banned_at IS NOT NULL FROM accounts WHERE id = $1 FOR NO KEY UPDATE",
    )
    .bind(target)
    .fetch_optional(&mut *tx)
    .await?;
    // Deleted, maybe while we waited for the row: checked in its own
    // statement, which sees what committed during the wait.
    let deleted: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM deleted_accounts WHERE id = $1)")
            .bind(target)
            .fetch_one(&mut *tx)
            .await?;
    let Some(already) = row.filter(|_| !deleted) else {
        return Err(err(
            ErrorCode::AccountNotFound,
            format!("No account with ID {target}."),
        ));
    };
    if already {
        return Ok(Banned::default());
    }
    sqlx::query("UPDATE accounts SET banned_at = now() WHERE id = $1")
        .bind(target)
        .execute(&mut *tx)
        .await?;
    let devices: Vec<Uuid> = sqlx::query_scalar(
        "UPDATE devices SET revoked_at = now() WHERE account_id = $1 AND revoked_at IS NULL RETURNING id",
    )
    .bind(target)
    .fetch_all(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE agents SET online = false, last_online_at = now() WHERE account_id = $1 AND online",
    )
    .bind(target)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE web_sessions SET revoked_at = now() WHERE account_id = $1 AND revoked_at IS NULL",
    )
    .bind(target)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE app_sessions SET revoked_at = now(), push_token = NULL
         WHERE account_id = $1 AND revoked_at IS NULL",
    )
    .bind(target)
    .execute(&mut *tx)
    .await?;
    let canceled: Vec<(i64, Uuid)> = sqlx::query_as(
        "UPDATE deliveries d SET state = 'canceled', canceled_at = now(), cancel_reason = 'account_banned'
         FROM messages m
         WHERE d.message_id = m.id AND (m.from_account = $1 OR d.account_id = $1)
           AND d.state IN ('pending', 'pushed')
         RETURNING d.message_id, d.agent_id",
    )
    .bind(target)
    .fetch_all(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE requests SET status = 'declined', decided_at = now()
         WHERE status = 'pending' AND (from_account = $1 OR to_account = $1)",
    )
    .bind(target)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE reports SET status = 'actioned', action = 'ban', resolved_by = $2, resolved_at = now()
         WHERE target_id = $1 AND status = 'open'",
    )
    .bind(target)
    .bind(by)
    .execute(&mut *tx)
    .await?;
    sqlx::query("SELECT pg_notify($1, $2)")
        .bind(BAN_CHANNEL)
        .bind(target.to_string())
        .execute(&mut *tx)
        .await?;
    Ok(Banned {
        newly: true,
        devices,
        canceled,
    })
}

/// Cancel the account's Waffo subscriptions that still renew, without a
/// refund. Nothing to cancel, or no Waffo configured, counts as done; App
/// Store subscriptions only their owner can cancel.
pub async fn cancel_subscriptions(
    db: &PgPool,
    cfg: &Config,
    http: &reqwest::Client,
    account: i32,
) -> ApiResult<()> {
    let Some(waffo) = &cfg.waffo else {
        return Ok(());
    };
    let live: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM subscriptions
         WHERE account_id = $1 AND mode = $2 AND provider = 'waffo'
           AND status IN ('active', 'past_due')",
    )
    .bind(account)
    .bind(waffo.mode())
    .fetch_all(db)
    .await?;
    for id in live {
        billing::cancel_order(http, waffo, &id).await?;
    }
    Ok(())
}

/// After a ban commits: cancel the Waffo subscriptions, logging a failure;
/// the ban stands either way (ADR-041). A first ban of an account with an
/// App Store subscription that still renews mails it how to cancel that
/// (API-066). Returns whether the Waffo subscriptions are canceled, and
/// whether there is such an App Store subscription.
pub async fn after_ban(
    db: &PgPool,
    cfg: &Config,
    http: &reqwest::Client,
    mailer: &Mailer,
    account: i32,
    newly: bool,
) -> (bool, bool) {
    let canceled = match cancel_subscriptions(db, cfg, http, account).await {
        Ok(()) => true,
        Err(e) => {
            warn!(account_id = account, error = %e.0.detail, "ban: subscription not canceled");
            false
        }
    };
    let app_store = match apple::renewing(db, cfg, account).await {
        Ok(subs) => !subs.is_empty(),
        Err(e) => {
            warn!(account_id = account, error = %e, "ban: App Store subscriptions not checked");
            false
        }
    };
    if app_store && newly {
        let email: Result<String, _> =
            sqlx::query_scalar("SELECT email FROM accounts WHERE id = $1")
                .bind(account)
                .fetch_one(db)
                .await;
        let sent = match email {
            Ok(email) => mailer.send_suspension(&email, account).await,
            Err(e) => Err(e.into()),
        };
        if let Err(e) = sent {
            warn!(account_id = account, error = %e, "ban: App Store letter not sent");
        }
    }
    info!(
        event = "account_banned",
        account_id = account,
        newly,
        subscription_canceled = canceled,
        app_store_subscription = app_store,
        "account banned"
    );
    (canceled, app_store)
}

/// API-067's write: clear the ban. Sign-ins, subscriptions and messages
/// taken back stay that way.
pub async fn unban(db: &PgPool, target: i32) -> ApiResult<()> {
    let r = sqlx::query(
        "UPDATE accounts a SET banned_at = NULL
         WHERE id = $1 AND NOT EXISTS (SELECT 1 FROM deleted_accounts d WHERE d.id = a.id)",
    )
    .bind(target)
    .execute(db)
    .await?;
    if r.rows_affected() == 0 {
        return Err(err(
            ErrorCode::AccountNotFound,
            format!("No account with ID {target}."),
        ));
    }
    info!(
        event = "account_unbanned",
        account_id = target,
        "account unbanned"
    );
    Ok(())
}

/// Close the connections of accounts banned by `imbo-server account ban`
/// in another process, and push the take-backs its transaction made.
/// A ban through API-064 or API-066 does both itself; doing them twice is
/// harmless.
pub fn spawn_ban_listener(st: crate::state::St) {
    tokio::spawn(async move {
        loop {
            if let Err(e) = listen_bans(&st).await {
                warn!(error = %e, "ban listener");
            }
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        }
    });
}

async fn listen_bans(st: &crate::state::St) -> Result<(), sqlx::Error> {
    let mut listener = sqlx::postgres::PgListener::connect_with(&st.db).await?;
    listener.listen(BAN_CHANNEL).await?;
    loop {
        let n = listener.recv().await?;
        let Ok(account) = n.payload().parse::<i32>() else {
            continue;
        };
        st.hub.close_account(
            account,
            imbo_proto::frames::CLOSE_UNAUTHORIZED,
            "account banned",
        );
        let canceled: Vec<(i64, Uuid)> = sqlx::query_as(
            "SELECT d.message_id, d.agent_id FROM deliveries d JOIN messages m ON m.id = d.message_id
             WHERE d.state = 'canceled' AND d.cancel_reason = 'account_banned'
               AND d.canceled_at > now() - interval '5 minutes'
               AND (m.from_account = $1 OR d.account_id = $1)",
        )
        .bind(account)
        .fetch_all(&st.db)
        .await?;
        crate::delivery::notify_canceled(st, canceled).await;
    }
}
