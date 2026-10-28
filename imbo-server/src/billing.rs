//! Charging: who may send (RULE-007, RULE-008), Creem calls and webhooks
//! (API-033–035, ADR-019–021).

use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use imbo_proto::ErrorCode;
use imbo_proto::api::{AccessView, SubscriptionView};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgConnection, PgPool};
use tracing::{info, warn};

use crate::config::{Config, Creem};
use crate::error::{ApiError, ApiResult, err};
use crate::state::St;

pub const TRIAL: TimeDelta = TimeDelta::days(30);
/// Active and past-due subscriptions keep access this long after the
/// paid-through date, while Creem retries the renewal.
pub const PAST_DUE_GRACE: TimeDelta = TimeDelta::days(3);
const CREEM_TIMEOUT: Duration = Duration::from_secs(10);

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
    pub account_id: i32,
    pub customer_id: String,
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
        }
    }
}

/// These accounts' subscriptions in the server's current mode, newest first.
pub async fn subscriptions(
    db: &PgPool,
    cfg: &Config,
    accounts: &[i32],
) -> Result<Vec<SubRow>, sqlx::Error> {
    let Some(creem) = &cfg.creem else {
        return Ok(vec![]);
    };
    sqlx::query_as(
        "SELECT account_id, customer_id, plan, status, paid_until, revoked_at FROM subscriptions
         WHERE account_id = ANY($1) AND mode = $2 ORDER BY creem_updated_at DESC",
    )
    .bind(accounts)
    .bind(creem.mode())
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

// ---- Creem API ----

fn provider_failed() -> ApiError {
    err(
        ErrorCode::PaymentProviderFailed,
        "The payment service is not answering. Try again shortly.",
    )
}

pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(CREEM_TIMEOUT)
        .build()
        .expect("build HTTP client")
}

async fn creem_call(creem: &Creem, req: reqwest::RequestBuilder, what: &str) -> ApiResult<Value> {
    let res = req
        .header("x-api-key", &creem.api_key)
        .send()
        .await
        .map_err(|e| {
            warn!(error = %e, what, "creem request failed");
            provider_failed()
        })?;
    let status = res.status();
    let body: Value = res.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        warn!(%status, what, trace = %body["trace_id"], message = %body["message"], "creem refused");
        return Err(provider_failed());
    }
    Ok(body)
}

/// API-033: a hosted checkout for `plan`, paid by `email`, tied to `account`.
pub async fn create_checkout(
    st: &St,
    creem: &Creem,
    account: i32,
    email: &str,
    plan: &str,
) -> ApiResult<String> {
    let product = if plan == "yearly" {
        &creem.product_yearly
    } else {
        &creem.product_monthly
    };
    let success = format!(
        "{}/account?checkout=done",
        st.cfg.web_origin.as_deref().unwrap_or("")
    );
    let mut body = json!({
        "product_id": product,
        "customer": { "email": email },
        "metadata": { "account_id": account.to_string() },
    });
    if success.starts_with("http") {
        body["success_url"] = json!(success);
    }
    let req = st
        .http
        .post(format!("{}/v1/checkouts", creem.api_base()))
        .json(&body);
    let v = creem_call(creem, req, "checkout").await?;
    v["checkout_url"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(provider_failed)
}

/// API-034: Creem's customer page for `customer`.
pub async fn portal_link(st: &St, creem: &Creem, customer: &str) -> ApiResult<String> {
    let req = st
        .http
        .post(format!("{}/v1/customers/billing", creem.api_base()))
        .json(&json!({ "customer_id": customer }));
    let v = creem_call(creem, req, "portal").await?;
    v["customer_portal_link"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(provider_failed)
}

/// API-052, API-066, API-070: end a subscription now, without a refund.
pub async fn cancel_subscription(http: &reqwest::Client, creem: &Creem, id: &str) -> ApiResult<()> {
    let req = http
        .post(format!("{}/v1/subscriptions/{id}/cancel", creem.api_base()))
        .json(&json!({ "mode": "immediate" }));
    creem_call(creem, req, "cancel").await?;
    Ok(())
}

/// `imbo-server billing pull`: Creem's current record of a subscription.
pub async fn fetch_subscription(
    http: &reqwest::Client,
    creem: &Creem,
    id: &str,
) -> ApiResult<Value> {
    if id.is_empty() || !id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(err(
            ErrorCode::InvalidRequest,
            "A subscription ID looks like sub_…",
        ));
    }
    let req = http.get(format!(
        "{}/v1/subscriptions?subscription_id={id}",
        creem.api_base()
    ));
    creem_call(creem, req, "subscription").await
}

// ---- webhooks (API-035) ----

fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut k = [0u8; BLOCK];
    if key.len() > BLOCK {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let ipad: Vec<u8> = k.iter().map(|b| b ^ 0x36).collect();
    let opad: Vec<u8> = k.iter().map(|b| b ^ 0x5c).collect();
    let inner = Sha256::new()
        .chain_update(&ipad)
        .chain_update(msg)
        .finalize();
    Sha256::new()
        .chain_update(&opad)
        .chain_update(inner)
        .finalize()
        .into()
}

/// `creem-signature` is the hex HMAC-SHA256 of the raw body.
pub fn signature_ok(secret: &str, body: &[u8], signature: &str) -> bool {
    let want = hex::encode(hmac_sha256(secret.as_bytes(), body));
    let got = signature.trim().to_ascii_lowercase();
    want.len() == got.len()
        && want
            .bytes()
            .zip(got.bytes())
            .fold(0u8, |acc, (a, b)| acc | (a ^ b))
            == 0
}

/// What a webhook did, for the log.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    Applied,
    Duplicate,
    /// Older than what is recorded.
    Stale,
    UnknownAccount,
    WrongMode,
    /// A product other than the two plans.
    UnknownProduct,
    /// A type that doesn't change subscriptions.
    Stored,
}

impl Outcome {
    /// The outcome names of `EVT:billing_event` (DESIGN §20).
    pub fn as_str(&self) -> &'static str {
        match self {
            Outcome::Applied => "applied",
            Outcome::Duplicate => "duplicate",
            Outcome::Stale => "stale",
            Outcome::UnknownAccount | Outcome::UnknownProduct => "unmapped",
            Outcome::WrongMode | Outcome::Stored => "ignored",
        }
    }
}

/// An id that Creem sends either as a string or as an expanded object.
fn id_of(v: &Value) -> Option<String> {
    v.as_str()
        .map(str::to_string)
        .or_else(|| v["id"].as_str().map(str::to_string))
}

fn time_of(v: &Value) -> Option<DateTime<Utc>> {
    v.as_str()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&Utc))
}

pub fn event_subscription(kind: &str, obj: &Value) -> Option<String> {
    match kind {
        "checkout.completed" => id_of(&obj["subscription"]),
        k if k.starts_with("subscription.") => id_of(obj),
        _ => id_of(&obj["subscription"]).or_else(|| id_of(&obj["transaction"]["subscription"])),
    }
}

/// Record a subscription object as Creem describes it, with the account from
/// its metadata. `keep_paid_until` leaves the paid-through date alone
/// (past-due, expired, paused).
pub async fn upsert_subscription(
    tx: &mut PgConnection,
    creem: &Creem,
    sub: &Value,
    keep_paid_until: bool,
    fallback_time: DateTime<Utc>,
) -> Result<Outcome, sqlx::Error> {
    let Some(id) = id_of(sub) else {
        return Ok(Outcome::Stored);
    };
    let customer = id_of(&sub["customer"]).unwrap_or_default();
    let product = id_of(&sub["product"]).unwrap_or_default();
    let metadata = &sub["metadata"];
    let Some(plan) = creem.plan_of(&product) else {
        return Ok(Outcome::UnknownProduct);
    };
    let status = sub["status"].as_str().unwrap_or("active").to_string();
    let updated = time_of(&sub["updated_at"]).unwrap_or(fallback_time);
    let period_end = time_of(&sub["current_period_end_date"]);

    let existing: Option<i32> =
        sqlx::query_scalar("SELECT account_id FROM subscriptions WHERE id = $1 FOR UPDATE")
            .bind(&id)
            .fetch_optional(&mut *tx)
            .await?;
    let from_metadata = metadata["account_id"]
        .as_str()
        .and_then(|s| s.parse::<i32>().ok())
        .or_else(|| metadata["account_id"].as_i64().map(|n| n as i32));
    let account = match (existing, from_metadata) {
        (Some(a), _) => a,
        (None, Some(a)) => {
            let known: bool =
                sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM accounts WHERE id = $1)")
                    .bind(a)
                    .fetch_one(&mut *tx)
                    .await?;
            if !known {
                return Ok(Outcome::UnknownAccount);
            }
            a
        }
        (None, None) => return Ok(Outcome::UnknownAccount),
    };
    // A first record without a period end (checkout.completed) is paid through
    // now; the renewal grace covers it until subscription.paid brings the end.
    let changed = sqlx::query(
        "INSERT INTO subscriptions (id, mode, account_id, customer_id, plan, status, paid_until, creem_updated_at)
         VALUES ($1, $2, $3, $4, $5, $6, coalesce($7, now()), $8)
         ON CONFLICT (id) DO UPDATE SET
           customer_id = CASE WHEN EXCLUDED.customer_id = '' THEN subscriptions.customer_id ELSE EXCLUDED.customer_id END,
           plan = EXCLUDED.plan,
           status = EXCLUDED.status,
           paid_until = CASE WHEN $9 THEN subscriptions.paid_until ELSE coalesce($7, subscriptions.paid_until) END,
           creem_updated_at = EXCLUDED.creem_updated_at
         WHERE subscriptions.creem_updated_at <= EXCLUDED.creem_updated_at",
    )
    .bind(&id)
    .bind(creem.mode())
    .bind(account)
    .bind(&customer)
    .bind(plan)
    .bind(&status)
    .bind(period_end)
    .bind(updated)
    .bind(keep_paid_until)
    .execute(&mut *tx)
    .await?;
    Ok(if changed.rows_affected() == 0 {
        Outcome::Stale
    } else {
        Outcome::Applied
    })
}

/// Store one verified webhook and apply it, in one transaction (INV-015).
pub async fn handle_event(st: &St, creem: &Creem, event: &Value) -> ApiResult<Outcome> {
    let (Some(id), Some(kind)) = (event["id"].as_str(), event["eventType"].as_str()) else {
        return Err(err(
            ErrorCode::InvalidRequest,
            "Expected a Creem event with id and eventType.",
        ));
    };
    let obj = &event["object"];
    let sub_id = event_subscription(kind, obj);
    let received = event["created_at"]
        .as_i64()
        .and_then(DateTime::from_timestamp_millis)
        .unwrap_or_else(Utc::now);

    let mut tx = st.db.begin().await?;
    let inserted = sqlx::query(
        "INSERT INTO billing_events (id, type, subscription_id, payload) VALUES ($1, $2, $3, $4)
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(id)
    .bind(kind)
    .bind(&sub_id)
    .bind(sqlx::types::Json(event))
    .execute(&mut *tx)
    .await?;
    if inserted.rows_affected() == 0 {
        return Ok(Outcome::Duplicate);
    }
    // Creem calls live mode `prod`; anything else is a test mode.
    let live = obj["mode"].as_str() == Some("prod");
    let outcome = if live != (creem.mode() == "live") {
        Outcome::WrongMode
    } else {
        match kind {
            "checkout.completed" if obj["subscription"].is_object() => {
                // The checkout carries the customer, product and our metadata.
                let mut sub = obj["subscription"].clone();
                for key in ["customer", "product", "metadata"] {
                    if !obj[key].is_null() {
                        sub[key] = obj[key].clone();
                    }
                }
                upsert_subscription(&mut tx, creem, &sub, false, received).await?
            }
            "subscription.active"
            | "subscription.paid"
            | "subscription.update"
            | "subscription.scheduled_cancel"
            | "subscription.canceled" => {
                upsert_subscription(&mut tx, creem, obj, false, received).await?
            }
            "subscription.past_due" | "subscription.expired" | "subscription.paused" => {
                upsert_subscription(&mut tx, creem, obj, true, received).await?
            }
            "refund.created" | "dispute.created" => {
                let r = sqlx::query(
                    "UPDATE subscriptions SET revoked_at = coalesce(revoked_at, now()) WHERE id = $1",
                )
                .bind(&sub_id)
                .execute(&mut *tx)
                .await?;
                if r.rows_affected() == 0 {
                    Outcome::UnknownAccount
                } else {
                    Outcome::Applied
                }
            }
            _ => Outcome::Stored,
        }
    };
    tx.commit().await?;
    if outcome == Outcome::Applied
        && (kind.starts_with("subscription.") || kind.starts_with("checkout."))
        && let Some(sub) = &sub_id
        && let Err(e) = warn_if_duplicate(st, sub).await
    {
        warn!(error = %e, "duplicate subscription check failed");
    }
    Ok(outcome)
}

/// Two subscriptions granting access to one account means one was paid twice
/// (DESIGN §16); ops refunds one by RB-005.
async fn warn_if_duplicate(st: &St, sub_id: &str) -> Result<(), sqlx::Error> {
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
        .filter(|s| s.grants_until().is_some_and(|g| g > now))
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

    #[test]
    fn hmac_matches_rfc4231_case_2() {
        let mac = hmac_sha256(b"Jefe", b"what do ya want for nothing?");
        assert_eq!(
            hex::encode(mac),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }

    #[test]
    fn long_keys_are_hashed_first() {
        // RFC 4231 test case 6: a 131-byte key.
        let key = [0xaau8; 131];
        let mac = hmac_sha256(
            &key,
            b"Test Using Larger Than Block-Size Key - Hash Key First",
        );
        assert_eq!(
            hex::encode(mac),
            "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
        );
    }

    #[test]
    fn signature_compares_hex() {
        let sig = hex::encode(hmac_sha256(b"s", b"{}"));
        assert!(signature_ok("s", b"{}", &sig));
        assert!(signature_ok("s", b"{}", &sig.to_uppercase()));
        assert!(!signature_ok("t", b"{}", &sig));
        assert!(!signature_ok("s", b"{}", ""));
    }

    fn cfg_on() -> Config {
        Config {
            creem: Some(Creem {
                api_key: "creem_test_x".into(),
                webhook_secret: "s".into(),
                product_monthly: "prod_m".into(),
                product_yearly: "prod_y".into(),
            }),
            billing_start: Some("2026-11-01T00:00:00Z".parse().unwrap()),
            ..Config::default()
        }
    }

    fn sub(status: &str, paid_until: &str, revoked: bool) -> SubRow {
        SubRow {
            account_id: 1,
            customer_id: "cust".into(),
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
        // Refunded or disputed: nothing, even inside the period.
        let s = sub("active", "2026-12-10T00:00:00Z", true);
        assert!(!decide(&cfg, t("2026-12-05T00:00:00Z"), early, &[&s]).active());
        // Paused or expired: nothing.
        let s = sub("paused", "2026-12-10T00:00:00Z", false);
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
