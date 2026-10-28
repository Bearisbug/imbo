//! Charging: who may send (RULE-007, RULE-008), Waffo calls and webhooks
//! (API-033–035, ADR-019–021, ADR-042).

use std::collections::HashMap;
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use chrono::{DateTime, NaiveDate, TimeDelta, Utc};
use imbo_proto::ErrorCode;
use imbo_proto::api::{AccessView, SubscriptionView};
use ring::rand::SystemRandom;
use ring::signature::{RSA_PKCS1_2048_8192_SHA256, RSA_PKCS1_SHA256, UnparsedPublicKey};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::{PgConnection, PgPool};
use tracing::{info, warn};

use crate::config::{Config, Waffo, rsa_key_pair};
use crate::error::{ApiError, ApiResult, err};
use crate::state::St;

pub const TRIAL: TimeDelta = TimeDelta::days(30);
/// Active and past-due subscriptions keep access this long after the
/// paid-through date, while Waffo retries the renewal.
pub const PAST_DUE_GRACE: TimeDelta = TimeDelta::days(3);
const WAFFO_TIMEOUT: Duration = Duration::from_secs(10);
const WAFFO_API: &str = "https://api.waffo.ai";

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
    let Some(waffo) = &cfg.waffo else {
        return Ok(vec![]);
    };
    sqlx::query_as(
        "SELECT account_id, plan, status, paid_until, revoked_at FROM subscriptions
         WHERE account_id = ANY($1) AND mode = $2 ORDER BY provider_updated_at DESC",
    )
    .bind(accounts)
    .bind(waffo.mode())
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

// ---- Waffo API ----

fn provider_failed() -> ApiError {
    err(
        ErrorCode::PaymentProviderFailed,
        "The payment service is not answering. Try again shortly.",
    )
}

pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(WAFFO_TIMEOUT)
        .build()
        .expect("build HTTP client")
}

/// Why a Waffo call returned no data.
#[derive(Debug)]
enum Refusal {
    /// Waffo answered with this error.
    Said(String),
    /// No answer, or an answer without data.
    Silent,
}

impl From<Refusal> for ApiError {
    fn from(_: Refusal) -> Self {
        provider_failed()
    }
}

/// `X-Signature`: RSA-SHA256 of `POST\n<path>\n<timestamp>\n<base64 SHA-256 of the body>`.
fn request_signature(waffo: &Waffo, path: &str, timestamp: &str, body: &[u8]) -> Option<String> {
    let canonical = format!(
        "POST\n{path}\n{timestamp}\n{}",
        STANDARD.encode(Sha256::digest(body))
    );
    let key = rsa_key_pair(&waffo.key)?;
    let mut sig = vec![0; key.public().modulus_len()];
    key.sign(
        &RSA_PKCS1_SHA256,
        &SystemRandom::new(),
        canonical.as_bytes(),
        &mut sig,
    )
    .ok()?;
    Some(STANDARD.encode(sig))
}

/// A signed call to Waffo's API; the response envelope's `data`.
async fn waffo_post(
    http: &reqwest::Client,
    waffo: &Waffo,
    path: &str,
    body: &Value,
    what: &str,
) -> Result<Value, Refusal> {
    let body = body.to_string().into_bytes();
    let timestamp = Utc::now().timestamp().to_string();
    let signature = request_signature(waffo, path, &timestamp, &body).ok_or_else(|| {
        warn!(what, "signing a waffo request failed");
        Refusal::Silent
    })?;
    let res = http
        .post(format!("{WAFFO_API}{path}"))
        .header("content-type", "application/json")
        .header("x-merchant-id", &waffo.merchant_id)
        .header("x-timestamp", timestamp)
        .header("x-signature", signature)
        .body(body)
        .send()
        .await
        .map_err(|e| {
            warn!(error = %e, what, "waffo request failed");
            Refusal::Silent
        })?;
    let status = res.status();
    let mut envelope: Value = res.json().await.unwrap_or(Value::Null);
    if let Some(message) = envelope["errors"][0]["message"].as_str() {
        warn!(%status, what, message, "waffo refused");
        return Err(Refusal::Said(message.to_string()));
    }
    if !status.is_success() || envelope["data"].is_null() {
        warn!(%status, what, "waffo refused");
        return Err(Refusal::Silent);
    }
    Ok(envelope["data"].take())
}

/// API-033: a hosted checkout for `plan`, paid by `email`, tied to `account`.
/// `with_trial` puts the first charge 30 days out (ADR-019).
pub async fn create_checkout(
    st: &St,
    waffo: &Waffo,
    account: i32,
    email: &str,
    plan: &str,
    with_trial: bool,
) -> ApiResult<String> {
    let product = waffo.product(plan);
    let identity = account.to_string();
    let mut session = json!({
        "productId": product,
        "currency": "USD",
        "buyerEmail": email,
        "expiresInSeconds": 3600,
        "metadata": { "account_id": identity },
        "withTrial": with_trial,
    });
    let success = format!(
        "{}/account?checkout=done",
        st.cfg.web_origin.as_deref().unwrap_or("")
    );
    // The payment page refuses any other return address when the buyer pays.
    if success.starts_with("https://") {
        session["successUrl"] = json!(success);
    }
    let token = json!({ "productId": product, "buyerIdentity": identity });
    let (token, session) = tokio::try_join!(
        waffo_post(
            &st.http,
            waffo,
            "/v1/actions/auth/issue-session-token",
            &token,
            "session token"
        ),
        waffo_post(
            &st.http,
            waffo,
            "/v1/actions/checkout/create-session",
            &session,
            "checkout"
        ),
    )?;
    // The token signs in as the buyer: never log the URL.
    match (session["checkoutUrl"].as_str(), token["token"].as_str()) {
        (Some(url), Some(token)) => Ok(format!("{url}#token={token}")),
        _ => {
            warn!("waffo checkout without a URL or token");
            Err(provider_failed())
        }
    }
}

/// API-034: Waffo's customer page for `account`, signed in by the URL.
pub async fn portal_link(st: &St, waffo: &Waffo, account: i32) -> ApiResult<String> {
    let body = json!({
        "storeId": waffo.store_id,
        "buyerIdentity": account.to_string(),
        "purpose": "portal",
    });
    let v = waffo_post(
        &st.http,
        waffo,
        "/v1/actions/auth/issue-session-token",
        &body,
        "portal",
    )
    .await?;
    match (v["portalUrl"].as_str(), v["token"].as_str()) {
        (Some(url), Some(token)) => Ok(format!("{url}#token={token}")),
        _ => {
            warn!("waffo portal without a URL or token");
            Err(provider_failed())
        }
    }
}

/// API-052, API-066, API-070, RB-007: stop renewing, without a refund. An
/// active order ends at its period end, a past-due one at once. Waffo
/// refusing because the order is already canceled counts as canceled.
pub async fn cancel_order(http: &reqwest::Client, waffo: &Waffo, id: &str) -> ApiResult<()> {
    let body = json!({ "orderId": id });
    match waffo_post(
        http,
        waffo,
        "/v1/actions/subscription-order/cancel-order",
        &body,
        "cancel",
    )
    .await
    {
        Ok(_) => Ok(()),
        Err(Refusal::Said(m)) if already_canceled(&m) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// `Subscription cannot be canceled, current status: <status>` for an order
/// that no longer renews.
fn already_canceled(message: &str) -> bool {
    message
        .split_once("current status: ")
        .is_some_and(|(_, s)| matches!(s.trim(), "canceling" | "canceled" | "expired" | "closed"))
}

/// An order ID as Waffo writes them, `ORD_…`.
pub fn order_id_ok(id: &str) -> bool {
    id.len() > 4
        && id.starts_with("ORD_")
        && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// `imbo-server billing pull`: Waffo's current record of a subscription order.
pub async fn fetch_order(http: &reqwest::Client, waffo: &Waffo, id: &str) -> ApiResult<Value> {
    if !order_id_ok(id) {
        return Err(err(
            ErrorCode::InvalidRequest,
            "An order ID looks like ORD_…",
        ));
    }
    let query = json!({
        "query": "query($id: String!) { subscriptionOrder(id: $id) { id status billingPeriod \
                  currentPeriodEnd updatedAt metadata merchantProvidedBuyerIdentity } }",
        "variables": { "id": id },
    });
    let mut v = waffo_post(http, waffo, "/v1/graphql", &query, "subscription order").await?;
    let order = v["subscriptionOrder"].take();
    if order.is_null() {
        return Err(err(
            ErrorCode::NotFound,
            format!("Waffo has no subscription order {id} in this environment."),
        ));
    }
    Ok(order)
}

// ---- webhooks (API-035) ----

/// `X-Waffo-Signature: t=<ms>,v1=<base64>`: RSA-SHA256 of `<t>.<body>` under
/// `key`, with `t` at most 45 minutes old and 1 minute ahead of `now`.
pub fn signature_ok(key: &[u8], body: &[u8], header: &str, now: DateTime<Utc>) -> bool {
    let (mut t, mut v1) = (None, None);
    for pair in header.split(',') {
        match pair.split_once('=') {
            Some((k, v)) if k.trim() == "t" => t = Some(v.trim()),
            Some((k, v)) if k.trim() == "v1" => v1 = Some(v.trim()),
            _ => {}
        }
    }
    let (Some(t), Some(v1)) = (t, v1) else {
        return false;
    };
    let Ok(ms) = t.parse::<i64>() else {
        return false;
    };
    let age = now.timestamp_millis() - ms;
    if !(-60_000..=45 * 60_000).contains(&age) {
        return false;
    }
    let Ok(sig) = STANDARD.decode(v1) else {
        return false;
    };
    let signed = [t.as_bytes(), b".", body].concat();
    UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, key)
        .verify(&signed, &sig)
        .is_ok()
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
    /// A billing period other than the two plans.
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

fn time_of(v: &Value) -> Option<DateTime<Utc>> {
    v.as_str()
        .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
        .map(|t| t.with_timezone(&Utc))
}

/// `currentPeriodEnd` is a date in notices and a time in GraphQL; the period
/// ends at the end of that UTC day either way.
fn period_end(v: &Value) -> Option<DateTime<Utc>> {
    let day = match time_of(v) {
        Some(t) => t.date_naive(),
        None => NaiveDate::parse_from_str(v.as_str()?, "%Y-%m-%d").ok()?,
    };
    Some(day.succ_opt()?.and_hms_opt(0, 0, 0)?.and_utc())
}

/// The account in an order: our `account_id` metadata, else the buyer identity.
fn account_of(data: &Value) -> Option<i32> {
    let meta = &data["orderMetadata"]["account_id"];
    meta.as_str()
        .and_then(|s| s.parse().ok())
        .or_else(|| meta.as_i64().and_then(|n| i32::try_from(n).ok()))
        .or_else(|| {
            data["merchantProvidedBuyerIdentity"]
                .as_str()
                .and_then(|s| s.parse().ok())
        })
}

/// Record an order as `status`, from an event's `data` (or `billing pull`'s
/// copy of one), if `updated` is newer than what is recorded, or equal to it
/// and `apply_ties`. `keep_paid_until` leaves the paid-through date alone;
/// otherwise it moves to `currentPeriodEnd` when the data has one.
pub async fn upsert_subscription(
    tx: &mut PgConnection,
    waffo: &Waffo,
    data: &Value,
    status: &str,
    keep_paid_until: bool,
    updated: DateTime<Utc>,
    apply_ties: bool,
) -> Result<Outcome, sqlx::Error> {
    let Some(id) = data["orderId"].as_str() else {
        return Ok(Outcome::Stored);
    };
    let plan = match data["billingPeriod"].as_str() {
        Some(p @ ("monthly" | "yearly")) => p,
        _ => return Ok(Outcome::UnknownProduct),
    };
    let existing: Option<i32> =
        sqlx::query_scalar("SELECT account_id FROM subscriptions WHERE id = $1 FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut *tx)
            .await?;
    let account = match (existing, account_of(data)) {
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
    // A first record without a period end is paid through now; the renewal
    // grace covers it until an event brings the end.
    let changed = sqlx::query(
        "INSERT INTO subscriptions (id, mode, account_id, plan, status, paid_until, provider_updated_at)
         VALUES ($1, $2, $3, $4, $5, coalesce($6, now()), $7)
         ON CONFLICT (id) DO UPDATE SET
           plan = EXCLUDED.plan,
           status = EXCLUDED.status,
           paid_until = CASE WHEN $8 THEN subscriptions.paid_until ELSE coalesce($6, subscriptions.paid_until) END,
           provider_updated_at = EXCLUDED.provider_updated_at
         WHERE subscriptions.provider_updated_at < EXCLUDED.provider_updated_at
            OR ($9 AND subscriptions.provider_updated_at = EXCLUDED.provider_updated_at)",
    )
    .bind(id)
    .bind(waffo.mode())
    .bind(account)
    .bind(plan)
    .bind(status)
    .bind(period_end(&data["currentPeriodEnd"]))
    .bind(updated)
    .bind(keep_paid_until)
    .bind(apply_ties)
    .execute(&mut *tx)
    .await?;
    Ok(if changed.rows_affected() == 0 {
        Outcome::Stale
    } else {
        Outcome::Applied
    })
}

/// Record an order fetched by `billing pull` as API-035's table would.
/// `None` for the states that are never recorded.
pub async fn record_pulled(
    tx: &mut PgConnection,
    waffo: &Waffo,
    order: &Value,
) -> Result<Option<Outcome>, sqlx::Error> {
    let (status, keep) = match order["status"].as_str() {
        Some("active" | "trialing") => ("active", false),
        Some("past_due") => ("past_due", true),
        Some("canceling") => ("scheduled_cancel", false),
        Some("canceled") => ("canceled", true),
        _ => return Ok(None),
    };
    // Waffo returns the metadata as a JSON string.
    let metadata = order["metadata"]
        .as_str()
        .and_then(|s| serde_json::from_str::<Value>(s).ok())
        .unwrap_or_else(|| order["metadata"].clone());
    let data = json!({
        "orderId": order["id"],
        "orderMetadata": metadata,
        "merchantProvidedBuyerIdentity": order["merchantProvidedBuyerIdentity"],
        "billingPeriod": order["billingPeriod"],
        "currentPeriodEnd": order["currentPeriodEnd"],
    });
    let updated = time_of(&order["updatedAt"]).unwrap_or_else(Utc::now);
    Ok(Some(
        upsert_subscription(tx, waffo, &data, status, keep, updated, false).await?,
    ))
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

/// The account of a recorded subscription, if it is deleted or banned.
pub async fn gone_account(tx: &mut PgConnection, id: &str) -> Result<Option<i32>, sqlx::Error> {
    let gone: Option<(i32, bool)> = sqlx::query_as(
        "SELECT a.id, a.banned_at IS NOT NULL OR EXISTS (SELECT 1 FROM deleted_accounts d WHERE d.id = a.id)
         FROM subscriptions s JOIN accounts a ON a.id = s.account_id WHERE s.id = $1",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    Ok(gone.and_then(|(account, gone)| gone.then_some(account)))
}

/// A deleted or banned account's order renews again: it withdrew its
/// cancellation at Waffo's customer page, or paid a checkout opened before.
/// Cancel it (API-035).
async fn cancel_if_gone(
    tx: &mut PgConnection,
    st: &St,
    waffo: &Waffo,
    id: &str,
    why: &str,
) -> ApiResult<()> {
    let Some(account) = gone_account(tx, id).await? else {
        return Ok(());
    };
    cancel_order(&st.http, waffo, id).await.map_err(|_| {
        err(
            ErrorCode::Unavailable,
            "Waffo did not take the cancellation; deliver this again later.",
        )
    })?;
    info!(account_id = account, subscription_id = id, "{why}");
    Ok(())
}

/// Store one verified webhook and apply it, in one transaction (INV-015).
pub async fn handle_event(st: &St, waffo: &Waffo, event: &Value) -> ApiResult<Outcome> {
    let (Some(kind), Some(event_id)) = (event["eventType"].as_str(), event["eventId"].as_str())
    else {
        return Err(err(
            ErrorCode::InvalidRequest,
            "Expected a Waffo event with eventType and eventId.",
        ));
    };
    let data = &event["data"];
    let order = data["orderId"].as_str();
    let updated = time_of(&event["timestamp"]).unwrap_or_else(Utc::now);

    let mut tx = st.db.begin().await?;
    // One order's events can share an event ID, so the type is part of the key.
    let inserted = sqlx::query(
        "INSERT INTO billing_events (id, type, subscription_id, payload) VALUES ($1, $2, $3, $4)
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(format!("{kind}:{event_id}"))
    .bind(kind)
    .bind(order)
    .bind(sqlx::types::Json(event))
    .execute(&mut *tx)
    .await?;
    if inserted.rows_affected() == 0 {
        return Ok(Outcome::Duplicate);
    }
    let live = event["mode"].as_str() == Some("prod");
    let outcome = if live != waffo.live {
        Outcome::WrongMode
    } else {
        match kind {
            "subscription.activated" => {
                let o = upsert_subscription(&mut tx, waffo, data, "active", false, updated, true)
                    .await?;
                if o == Outcome::Applied
                    && let Some(id) = order
                {
                    let why = "deleted or banned account paid; canceled";
                    cancel_if_gone(&mut tx, st, waffo, id, why).await?;
                }
                o
            }
            "subscription.renewed" | "subscription.recovered" => {
                upsert_subscription(&mut tx, waffo, data, "active", false, updated, true).await?
            }
            "subscription.past_due" => {
                upsert_subscription(&mut tx, waffo, data, "past_due", true, updated, true).await?
            }
            "subscription.canceling" => {
                upsert_subscription(
                    &mut tx,
                    waffo,
                    data,
                    "scheduled_cancel",
                    false,
                    updated,
                    true,
                )
                .await?
            }
            "subscription.uncanceled" => {
                let o = upsert_subscription(&mut tx, waffo, data, "active", true, updated, true)
                    .await?;
                if matches!(o, Outcome::Applied | Outcome::Stale)
                    && let Some(id) = order
                {
                    let why = "deleted or banned account withdrew its cancellation; canceled again";
                    cancel_if_gone(&mut tx, st, waffo, id, why).await?;
                }
                o
            }
            "subscription.canceled" => {
                upsert_subscription(&mut tx, waffo, data, "canceled", true, updated, true).await?
            }
            "refund.succeeded" => match order {
                Some(id) if revoke(&mut tx, id).await?.is_some() => Outcome::Applied,
                _ => Outcome::UnknownAccount,
            },
            _ => Outcome::Stored,
        }
    };
    tx.commit().await?;
    if outcome == Outcome::Applied
        && kind.starts_with("subscription.")
        && let Some(id) = order
        && let Err(e) = warn_if_duplicate(st, id).await
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
    fn period_end_is_the_next_utc_midnight() {
        assert_eq!(
            period_end(&json!("2026-04-10")),
            Some(t("2026-04-11T00:00:00Z"))
        );
        assert_eq!(
            period_end(&json!("2026-04-10T22:58:48.000Z")),
            Some(t("2026-04-11T00:00:00Z"))
        );
        assert_eq!(period_end(&Value::Null), None);
    }

    #[test]
    fn account_from_metadata_then_buyer_identity() {
        assert_eq!(
            account_of(&json!({"orderMetadata": {"account_id": "12345678"}})),
            Some(12345678)
        );
        assert_eq!(
            account_of(&json!({"orderMetadata": {"account_id": 7}})),
            Some(7)
        );
        assert_eq!(
            account_of(&json!({"orderMetadata": {}, "merchantProvidedBuyerIdentity": "42"})),
            Some(42)
        );
        assert_eq!(
            account_of(&json!({"orderMetadata": {"planId": "pro"}})),
            None
        );
    }

    #[test]
    fn refusals_that_mean_canceled() {
        let m = |s| format!("Subscription cannot be canceled, current status: {s}");
        for s in ["canceling", "canceled", "expired", "closed"] {
            assert!(already_canceled(&m(s)), "{s}");
        }
        for s in ["trialing", "active", "past_due", "pending"] {
            assert!(!already_canceled(&m(s)), "{s}");
        }
        assert!(!already_canceled("Order not found"));
    }

    #[test]
    fn order_ids() {
        assert!(order_id_ok("ORD_5dXBtmF2HLlHfbPNm0Wcnz"));
        assert!(!order_id_ok("ORD_"));
        assert!(!order_id_ok("sub_123"));
        assert!(!order_id_ok("ORD_a/b"));
    }

    #[test]
    fn signature_header_needs_t_and_v1_in_time() {
        let now = t("2026-10-28T00:00:00Z");
        let key = [0u8; 4];
        assert!(!signature_ok(&key, b"{}", "", now));
        assert!(!signature_ok(&key, b"{}", "v1=AAAA", now));
        let old = now.timestamp_millis() - 46 * 60_000;
        assert!(!signature_ok(&key, b"{}", &format!("t={old},v1=AAAA"), now));
    }

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
            account_id: 1,
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
