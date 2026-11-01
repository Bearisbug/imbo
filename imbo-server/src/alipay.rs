//! Alipay time packs through Watcha (API-080, ADR-061). Watcha sends no
//! notification, so the server asks it: when the web app buys, when the
//! account page is read, and before a send is refused.

use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use serde_json::{Value, json};
use tracing::{info, warn};

use crate::billing::{self, SubRow, provider_failed};
use crate::config::Watcha;
use crate::error::ApiResult;
use crate::state::St;

/// Watcha answers within this or the call counts as failed.
const TIMEOUT: Duration = Duration::from_secs(5);
/// A pack longer than this is the yearly one; a calendar month is at most
/// 31 days.
const YEARLY: TimeDelta = TimeDelta::days(62);
/// The account page asks Watcha at most this often per account.
const PAGE_EVERY: Duration = Duration::from_secs(5);
/// Before refusing a send, at most this often per account.
const SEND_EVERY: Duration = Duration::from_secs(60);

/// A paid time pack, as Watcha dates it.
pub struct Pack {
    pub starts_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

impl Pack {
    /// The row ID: one pack per account and start time, so asking twice
    /// records one row (INV-033).
    fn id(&self, account: i32) -> String {
        format!("ALIPAY_{account}_{}", self.starts_at.timestamp())
    }

    fn plan(&self) -> &'static str {
        if self.expires_at - self.starts_at > YEARLY {
            "yearly"
        } else {
            "monthly"
        }
    }
}

/// A time pack as one subscription row. Pure, so the shape of the row is
/// readable without a database.
pub struct Row {
    pub id: String,
    pub plan: &'static str,
    pub paid_until: DateTime<Utc>,
}

/// `trial_end` is when this account's trial runs out, so a pack bought
/// inside the trial starts after it (ADR-061).
pub fn row(account: i32, pack: &Pack, trial_end: Option<DateTime<Utc>>) -> Row {
    let left = trial_end
        .map(|trial| trial - pack.starts_at)
        .filter(|left| *left > TimeDelta::zero())
        .unwrap_or_default();
    Row {
        id: pack.id(account),
        plan: pack.plan(),
        paid_until: pack.expires_at + left,
    }
}

pub enum Answer {
    /// Paid already.
    Granted(Pack),
    /// Not paid; this link takes the buyer to Alipay.
    PurchaseRequired(String),
}

/// Ask Watcha about this account's time pack. `return_url` is where the
/// buyer lands after paying.
pub async fn ask(
    st: &St,
    watcha: &Watcha,
    account: i32,
    return_url: Option<&str>,
) -> ApiResult<Answer> {
    let mut body = json!({
        "entitlement_id": watcha.entitlement_id,
        "user_id": account.to_string(),
    });
    if let Some(url) = return_url {
        body["return_url"] = json!(url);
    }
    let answer: Value = st
        .http
        .post(format!("{}/v1/entitlements/access", watcha.api_url()))
        .bearer_auth(&watcha.api_key)
        .timeout(TIMEOUT)
        .json(&body)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| {
            warn!(account_id = account, "Watcha refused the access check: {e}");
            provider_failed()
        })?
        .json()
        .await
        .map_err(|_| provider_failed())?;
    read(&answer).ok_or_else(|| {
        // Anything else — unavailable, a quota entitlement, an unknown shape
        // — fails closed, and the pack is not recorded.
        warn!(
            account_id = account,
            access = answer["access"].as_str().unwrap_or_default(),
            "Watcha answered something this server does not handle"
        );
        provider_failed()
    })
}

/// Unix seconds, not an ISO date.
fn at(v: &Value) -> Option<DateTime<Utc>> {
    DateTime::from_timestamp(v.as_i64()?, 0)
}

fn read(answer: &Value) -> Option<Answer> {
    match answer["access"].as_str()? {
        "granted" => {
            let e = &answer["entitlement"];
            Some(Answer::Granted(Pack {
                starts_at: at(&e["starts_at"])?,
                expires_at: at(&e["expires_at"])?,
            }))
        }
        "purchase_required" => Some(Answer::PurchaseRequired(
            answer["purchase"]["url"].as_str()?.to_string(),
        )),
        _ => None,
    }
}

/// Record a paid pack, once (INV-033). Returns what it grants access
/// through, whether this call recorded it or an earlier one did.
pub async fn record(st: &St, account: i32, pack: &Pack) -> Result<DateTime<Utc>, sqlx::Error> {
    let Some(waffo) = &st.cfg.waffo else {
        return Ok(pack.expires_at);
    };
    let created_at: DateTime<Utc> =
        sqlx::query_scalar("SELECT created_at FROM accounts WHERE id = $1")
            .bind(account)
            .fetch_one(&st.db)
            .await?;
    let want = row(account, pack, billing::trial_end(&st.cfg, created_at));
    let recorded: Option<DateTime<Utc>> = sqlx::query_scalar(
        "INSERT INTO subscriptions
           (id, provider, mode, account_id, plan, status, paid_until, provider_updated_at)
         VALUES ($1, 'alipay', $2, $3, $4, 'scheduled_cancel', $5, now())
         ON CONFLICT (id) DO NOTHING
         RETURNING paid_until",
    )
    .bind(&want.id)
    .bind(waffo.mode())
    .bind(account)
    .bind(want.plan)
    .bind(want.paid_until)
    .fetch_optional(&st.db)
    .await?;
    match recorded {
        Some(paid_until) => {
            info!(
                event = "alipay_pack_recorded",
                account_id = account,
                plan = want.plan,
                paid_until = %paid_until,
                mode = waffo.mode(),
                "alipay time pack recorded"
            );
            Ok(paid_until)
        }
        // Already recorded: keep what that row grants, revocation included.
        None => Ok(existing(st, &want.id).await?.unwrap_or(want.paid_until)),
    }
}

async fn existing(st: &St, id: &str) -> Result<Option<DateTime<Utc>>, sqlx::Error> {
    let found: Option<SubRow> = sqlx::query_as(
        "SELECT id, account_id, provider, mode, plan, status, paid_until, revoked_at
         FROM subscriptions WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&st.db)
    .await?;
    Ok(found.map(|r| r.grants_until().unwrap_or(r.paid_until)))
}

/// Which throttle a sync goes through.
#[derive(Clone, Copy)]
pub enum Sync {
    /// Reading the account page (API-032).
    Page,
    /// About to refuse a send (RULE-008).
    Send,
}

/// Ask Watcha whether this account paid since the last look, and record it.
/// Does nothing when Alipay packs are off, when the account already has a
/// subscription that grants access, or when it asked moments ago; a Watcha
/// failure only logs, so the caller answers from what is recorded.
pub async fn sync(st: &St, account: i32, when: Sync) {
    let Some(watcha) = &st.cfg.watcha else {
        return;
    };
    if !st.cfg.billing_on() {
        return;
    }
    let (key, every) = match when {
        Sync::Page => ("alipay-page", PAGE_EVERY),
        Sync::Send => ("alipay-send", SEND_EVERY),
    };
    if st
        .limiter
        .hit(&format!("{key}:{account}"), 1, every)
        .is_err()
    {
        return;
    }
    let now = Utc::now();
    match billing::subscriptions(&st.db, &st.cfg, &[account]).await {
        // A gift does not stand in the way: a gifted account that buys a
        // pack still gets it recorded (ADR-060).
        Ok(subs)
            if subs
                .iter()
                .any(|s| !s.is_gift() && s.grants_until().is_some_and(|g| g > now)) =>
        {
            return;
        }
        Ok(_) => {}
        Err(e) => {
            warn!(account_id = account, "cannot read subscriptions: {e}");
            return;
        }
    }
    match ask(st, watcha, account, None).await {
        Ok(Answer::Granted(pack)) => {
            if let Err(e) = record(st, account, &pack).await {
                warn!(account_id = account, "cannot record the time pack: {e}");
            }
        }
        // Not paid, or Watcha did not answer: both leave the records alone.
        Ok(Answer::PurchaseRequired(_)) | Err(_) => {}
    }
}

/// API-080: where to send a buyer who wants a pack.
pub async fn buy(st: &St, watcha: &Watcha, account: i32) -> ApiResult<Answer> {
    let return_url = st
        .cfg
        .web_origin
        .as_ref()
        .map(|o| format!("{o}/account?paid=alipay"));
    ask(st, watcha, account, return_url.as_deref()).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pack(days: i64) -> Pack {
        let starts_at = DateTime::from_timestamp(1_793_000_000, 0).unwrap();
        Pack {
            starts_at,
            expires_at: starts_at + TimeDelta::days(days),
        }
    }

    #[test]
    fn pack_shape() {
        // A calendar month is 28 to 31 days, a year 365 or 366.
        assert_eq!(pack(28).plan(), "monthly");
        assert_eq!(pack(31).plan(), "monthly");
        assert_eq!(pack(365).plan(), "yearly");
        // Asking twice about the same pack gives the same row ID (INV-033).
        assert_eq!(pack(31).id(7), "ALIPAY_7_1793000000");
        assert_eq!(pack(365).id(7), pack(31).id(7));
    }

    #[test]
    fn rows() {
        let p = pack(31);
        // Outside the trial the pack runs exactly as Watcha dates it.
        let r = row(7, &p, None);
        assert_eq!(r.id, "ALIPAY_7_1793000000");
        assert_eq!(r.plan, "monthly");
        assert_eq!(r.paid_until, p.expires_at);
        // Bought with 10 days of trial left: the pack starts after them.
        let trial = p.starts_at + TimeDelta::days(10);
        assert_eq!(
            row(7, &p, Some(trial)).paid_until,
            p.expires_at + TimeDelta::days(10)
        );
        // A trial that already ran out adds nothing.
        let over = p.starts_at - TimeDelta::days(3);
        assert_eq!(row(7, &p, Some(over)).paid_until, p.expires_at);
    }

    #[test]
    fn answers() {
        let granted = json!({
            "access": "granted",
            "entitlement": {"type": "duration", "starts_at": 1_793_000_000, "expires_at": 1_795_678_400},
        });
        let Some(Answer::Granted(p)) = read(&granted) else {
            panic!("a granted duration is a pack");
        };
        assert_eq!(p.starts_at.timestamp(), 1_793_000_000);
        assert_eq!(p.expires_at.timestamp(), 1_795_678_400);
        let required = json!({
            "access": "purchase_required",
            "entitlement": {"type": "duration"},
            "purchase": {"url": "https://pay.example/purchase?ticket=t"},
        });
        let Some(Answer::PurchaseRequired(url)) = read(&required) else {
            panic!("purchase_required carries the link");
        };
        assert_eq!(url, "https://pay.example/purchase?ticket=t");
        // Fail closed on everything else.
        assert!(read(&json!({"access": "unavailable", "reason": {"code": "x"}})).is_none());
        assert!(
            read(&json!({"access": "granted", "entitlement": {"type": "permanent"}})).is_none()
        );
        assert!(read(&json!({"access": "purchase_required"})).is_none());
        assert!(read(&json!({"access": "something_else"})).is_none());
        assert!(read(&json!({})).is_none());
    }
}
