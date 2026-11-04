//! Charging: who may send (RULE-007, RULE-008) and the subscriptions it
//! counts. The payment channels that record them are in `pay`.

use std::collections::HashMap;

use chrono::{DateTime, TimeDelta, Utc};
use imbo_proto::ErrorCode;
use imbo_proto::api::{AccessView, SubscriptionView};
use sqlx::{PgConnection, PgPool};
use tracing::{info, warn};

use crate::config::Config;
use crate::error::{ApiResult, err};
use crate::state::St;

pub const TRIAL: TimeDelta = TimeDelta::days(30);
/// Active and past-due subscriptions keep access this long after the
/// paid-through date, while the channel retries the renewal.
pub const PAST_DUE_GRACE: TimeDelta = TimeDelta::days(3);

#[derive(Clone, Debug, PartialEq)]
pub enum Access {
    Free,
    Trial(DateTime<Utc>),
    /// Paid through this date.
    Paid(DateTime<Utc>),
    /// Inactive since this time.
    Expired(DateTime<Utc>),
}

impl Access {
    pub fn active(&self) -> bool {
        !matches!(self, Access::Expired(_))
    }

    pub fn view(&self) -> AccessView {
        let (state, until) = match self {
            Access::Free => ("free", None),
            Access::Trial(t) => ("trial", Some(*t)),
            Access::Paid(t) => ("paid", Some(*t)),
            Access::Expired(t) => ("expired", Some(*t)),
        };
        AccessView {
            state: state.into(),
            until,
        }
    }
}

#[derive(sqlx::FromRow, Clone, Debug)]
pub struct SubRow {
    pub id: String,
    pub account_id: i32,
    /// The channel that records it: `waffo`, `apple`, `alipay` or `gift`.
    pub provider: String,
    pub mode: String,
    pub plan: String,
    pub status: String,
    pub paid_until: DateTime<Utc>,
    pub revoked_at: Option<DateTime<Utc>>,
}

impl SubRow {
    /// RULE-007: until when this subscription grants access, if at all.
    pub fn grants_until(&self) -> Option<DateTime<Utc>> {
        if self.revoked_at.is_some() {
            return None;
        }
        match self.status.as_str() {
            "active" | "past_due" => Some(self.paid_until + PAST_DUE_GRACE),
            "scheduled_cancel" | "canceled" => Some(self.paid_until),
            _ => None,
        }
    }

    pub fn view(&self) -> SubscriptionView {
        SubscriptionView {
            plan: self.plan.clone(),
            status: self.status.clone(),
            paid_until: self.paid_until,
            provider: self.provider.clone(),
        }
    }
}

/// These accounts' subscriptions in the server's current mode, newest first.
/// In live mode the review account's test subscriptions count too (RULE-007).
pub async fn subscriptions(
    db: &PgPool,
    cfg: &Config,
    accounts: &[i32],
) -> Result<Vec<SubRow>, sqlx::Error> {
    let Some(waffo) = &cfg.waffo else {
        return Ok(vec![]);
    };
    let review = cfg.review.as_ref().filter(|_| waffo.live).map(|r| &r.email);
    sqlx::query_as(
        "SELECT id, account_id, provider, mode, plan, status, paid_until, revoked_at
         FROM subscriptions
         WHERE account_id = ANY($1)
           AND (mode = $2 OR (mode = 'test' AND account_id IN
                (SELECT id FROM accounts WHERE lower(email) = $3::text)))
         ORDER BY provider_updated_at DESC",
    )
    .bind(accounts)
    .bind(waffo.mode())
    .bind(review)
    .fetch_all(db)
    .await
}

/// The trial ends 30 days after signing up or the billing start, whichever is later.
pub fn trial_end(cfg: &Config, created_at: DateTime<Utc>) -> Option<DateTime<Utc>> {
    cfg.billing_start.map(|start| created_at.max(start) + TRIAL)
}

fn decide(cfg: &Config, now: DateTime<Utc>, created_at: DateTime<Utc>, subs: &[&SubRow]) -> Access {
    if !cfg.billing_on() {
        return Access::Free;
    }
    let granting = subs
        .iter()
        .filter_map(|s| s.grants_until().map(|g| (g, s.paid_until)))
        .max();
    if let Some((until, paid)) = granting
        && until > now
    {
        return Access::Paid(paid);
    }
    let trial = trial_end(cfg, created_at).unwrap_or(now);
    if now < trial {
        return Access::Trial(trial);
    }
    Access::Expired(granting.map_or(trial, |(until, _)| until.max(trial)))
}

pub async fn access_many(
    db: &PgPool,
    cfg: &Config,
    accounts: &[i32],
) -> Result<HashMap<i32, Access>, sqlx::Error> {
    if !cfg.billing_on() {
        return Ok(accounts.iter().map(|a| (*a, Access::Free)).collect());
    }
    let rows: Vec<(i32, DateTime<Utc>)> =
        sqlx::query_as("SELECT id, created_at FROM accounts WHERE id = ANY($1)")
            .bind(accounts)
            .fetch_all(db)
            .await?;
    let subs = subscriptions(db, cfg, accounts).await?;
    let now = Utc::now();
    Ok(rows
        .into_iter()
        .map(|(id, created_at)| {
            let mine: Vec<&SubRow> = subs.iter().filter(|s| s.account_id == id).collect();
            (id, decide(cfg, now, created_at, &mine))
        })
        .collect())
}

pub async fn access(db: &PgPool, cfg: &Config, account: i32) -> Result<Access, sqlx::Error> {
    Ok(access_many(db, cfg, &[account])
        .await?
        .remove(&account)
        .unwrap_or(Access::Free))
}

fn subscribe_hint(cfg: &Config) -> String {
    match &cfg.web_origin {
        Some(o) => format!("Subscribe at {o}/account"),
        None => "Subscribe in the Imbo web app".into(),
    }
}

/// RULE-008: sending, uploading, adding contacts and creating or joining
/// groups need an active account.
pub async fn require_active(st: &St, account: i32, endpoint: &str) -> ApiResult<()> {
    if !st.cfg.billing_on() || access(&st.db, &st.cfg, account).await?.active() {
        return Ok(());
    }
    info!(
        event = "subscription_required",
        account_id = account,
        endpoint,
        "inactive account refused"
    );
    Err(err(
        ErrorCode::SubscriptionRequired,
        format!(
            "Your free trial has ended. {} to send again; your history stays readable.",
            subscribe_hint(&st.cfg)
        ),
    ))
}

/// One recorded subscription.
pub async fn row(db: &PgPool, id: &str) -> Result<Option<SubRow>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, account_id, provider, mode, plan, status, paid_until, revoked_at
         FROM subscriptions WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(db)
    .await
}

/// RULE-007: the subscription stops granting access from now on. Its account,
/// if it is recorded.
pub async fn revoke(tx: &mut PgConnection, id: &str) -> Result<Option<i32>, sqlx::Error> {
    sqlx::query_scalar(
        "UPDATE subscriptions SET revoked_at = coalesce(revoked_at, now()) WHERE id = $1
         RETURNING account_id",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await
}

/// Two paid subscriptions granting access to one account means one was paid
/// twice (DESIGN §16); ops refunds one by RB-005. A gift doesn't count.
pub async fn warn_if_duplicate(st: &St, sub_id: &str) -> Result<(), sqlx::Error> {
    let account: Option<i32> =
        sqlx::query_scalar("SELECT account_id FROM subscriptions WHERE id = $1")
            .bind(sub_id)
            .fetch_optional(&st.db)
            .await?;
    let Some(account) = account else {
        return Ok(());
    };
    let now = Utc::now();
    let granting = subscriptions(&st.db, &st.cfg, &[account])
        .await?
        .iter()
        .filter(|s| crate::pay::paid_grant(&st.cfg, s, now))
        .count();
    if granting > 1 {
        warn!(
            event = "duplicate_subscription",
            account_id = account,
            subscription_id = sub_id,
            "two subscriptions grant access; refund one (RB-005)"
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Waffo;

    fn cfg_on() -> Config {
        Config {
            waffo: Some(Waffo {
                merchant_id: "MER_x".into(),
                store_id: "STO_x".into(),
                key: vec![],
                live: false,
                product_monthly: "PROD_m".into(),
                product_yearly: "PROD_y".into(),
                webhook_key: vec![],
            }),
            billing_start: Some("2026-11-01T00:00:00Z".parse().unwrap()),
            ..Config::default()
        }
    }

    fn sub(status: &str, paid_until: &str, revoked: bool) -> SubRow {
        SubRow {
            id: "ORD_x".into(),
            account_id: 1,
            provider: "waffo".into(),
            mode: "test".into(),
            plan: "monthly".into(),
            status: status.into(),
            paid_until: paid_until.parse().unwrap(),
            revoked_at: revoked.then(Utc::now),
        }
    }

    fn t(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    #[test]
    fn rule_007() {
        let cfg = cfg_on();
        let early = t("2026-10-01T00:00:00Z");
        // Signed up before the start: 30 days from the start.
        assert_eq!(
            decide(&cfg, t("2026-11-20T00:00:00Z"), early, &[]),
            Access::Trial(t("2026-12-01T00:00:00Z"))
        );
        assert_eq!(
            decide(&cfg, t("2026-12-02T00:00:00Z"), early, &[]),
            Access::Expired(t("2026-12-01T00:00:00Z"))
        );
        // Past due: three more days after the paid-through date.
        let s = sub("past_due", "2026-12-10T00:00:00Z", false);
        assert_eq!(
            decide(&cfg, t("2026-12-12T00:00:00Z"), early, &[&s]),
            Access::Paid(t("2026-12-10T00:00:00Z"))
        );
        assert_eq!(
            decide(&cfg, t("2026-12-14T00:00:00Z"), early, &[&s]),
            Access::Expired(t("2026-12-13T00:00:00Z"))
        );
        // Cancelled: no grace.
        let s = sub("canceled", "2026-12-10T00:00:00Z", false);
        assert!(!decide(&cfg, t("2026-12-11T00:00:00Z"), early, &[&s]).active());
        // Refunded or revoked: nothing, even inside the period.
        let s = sub("active", "2026-12-10T00:00:00Z", true);
        assert!(!decide(&cfg, t("2026-12-05T00:00:00Z"), early, &[&s]).active());
        // Any other status: nothing.
        let s = sub("expired", "2026-12-10T00:00:00Z", false);
        assert!(!decide(&cfg, t("2026-12-05T00:00:00Z"), early, &[&s]).active());
        // Billing off: free.
        let mut off = cfg_on();
        off.billing_start = None;
        assert_eq!(
            decide(&off, t("2027-01-01T00:00:00Z"), early, &[]),
            Access::Free
        );
    }
}
