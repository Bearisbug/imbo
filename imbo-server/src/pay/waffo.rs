//! Waffo (API-033–035, ADR-019–021, ADR-042): its API, its webhooks, and
//! its adapter for the payment-channel port.

use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use chrono::{DateTime, NaiveDate, Utc};
use imbo_proto::ErrorCode;
use ring::rand::SystemRandom;
use ring::signature::{RSA_PKCS1_2048_8192_SHA256, RSA_PKCS1_SHA256, UnparsedPublicKey};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgConnection;
use tracing::{info, warn};

use super::{Channel, Ctx, Outcome, Stopper, provider_failed};
use crate::billing::{self, SubRow, revoke, warn_if_duplicate};
use crate::config::{Waffo, rsa_key_pair};
use crate::error::{ApiError, ApiResult, err};
use crate::state::St;

const WAFFO_API: &str = "https://api.waffo.ai";

/// Waffo on the payment-channel port. Its webhooks (API-035) come in
/// through `handle_event`.
pub struct Adapter<'a>(pub &'a Waffo);

#[async_trait]
impl Channel for Adapter<'_> {
    fn name(&self) -> &'static str {
        "waffo"
    }

    fn title(&self) -> &'static str {
        "Waffo"
    }

    fn owns(&self, id: &str) -> bool {
        order_id_ok(id)
    }

    fn stopper(&self) -> Stopper {
        Stopper::Server
    }

    async fn begin(&self, ctx: &Ctx<'_>, account: i32, plan: &str) -> ApiResult<String> {
        let email: String = sqlx::query_scalar("SELECT email FROM accounts WHERE id = $1")
            .bind(account)
            .fetch_one(ctx.db)
            .await?;
        // ADR-019: still in the trial, the first charge comes 30 days later.
        let with_trial = matches!(
            billing::access(ctx.db, ctx.cfg, account).await?,
            billing::Access::Trial(_)
        );
        let url = create_checkout(ctx, self.0, account, &email, plan, with_trial).await?;
        info!(
            event = "checkout_created",
            account_id = account,
            plan = %plan,
            mode = self.0.mode(),
            "checkout created"
        );
        Ok(url)
    }

    async fn portal(&self, ctx: &Ctx<'_>, account: i32) -> ApiResult<Option<String>> {
        portal_link(ctx, self.0, account).await.map(Some)
    }

    async fn pull(&self, ctx: &Ctx<'_>, id: &str) -> anyhow::Result<String> {
        pull(ctx, self.0, id).await
    }

    async fn stop(&self, ctx: &Ctx<'_>, sub: &SubRow) -> ApiResult<()> {
        // The review account's test order, kept in live mode (RULE-007), is
        // out of reach of the live keys.
        if sub.mode != self.0.mode() {
            return Ok(());
        }
        cancel_order(ctx.http, self.0, &sub.id).await
    }
}

// ---- Waffo API ----

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
async fn create_checkout(
    ctx: &Ctx<'_>,
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
        ctx.cfg.web_origin.as_deref().unwrap_or("")
    );
    // The payment page refuses any other return address when the buyer pays.
    if success.starts_with("https://") {
        session["successUrl"] = json!(success);
    }
    let token = json!({ "productId": product, "buyerIdentity": identity });
    let (token, session) = tokio::try_join!(
        waffo_post(
            ctx.http,
            waffo,
            "/v1/actions/auth/issue-session-token",
            &token,
            "session token"
        ),
        waffo_post(
            ctx.http,
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
async fn portal_link(ctx: &Ctx<'_>, waffo: &Waffo, account: i32) -> ApiResult<String> {
    let body = json!({
        "storeId": waffo.store_id,
        "buyerIdentity": account.to_string(),
        "purpose": "portal",
    });
    let v = waffo_post(
        ctx.http,
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
async fn cancel_order(http: &reqwest::Client, waffo: &Waffo, id: &str) -> ApiResult<()> {
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
fn order_id_ok(id: &str) -> bool {
    id.len() > 4
        && id.starts_with("ORD_")
        && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// `imbo-server billing pull`: Waffo's current record of a subscription order.
async fn fetch_order(http: &reqwest::Client, waffo: &Waffo, id: &str) -> ApiResult<Value> {
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
async fn upsert_subscription(
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
async fn record_pulled(
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

/// `imbo-server billing pull <order ID>` (RB-005): record the order as
/// API-035's table would, and cancel it when it still renews for a deleted
/// or banned account; the record stands even when Waffo can't cancel.
async fn pull(ctx: &Ctx<'_>, waffo: &Waffo, id: &str) -> anyhow::Result<String> {
    let order = fetch_order(ctx.http, waffo, id)
        .await
        .map_err(|e| anyhow::anyhow!("{}", e.0.detail))?;
    let mut tx = ctx.db.begin().await?;
    let outcome = record_pulled(&mut tx, waffo, &order).await?;
    let renews = matches!(
        order["status"].as_str(),
        Some("active" | "trialing" | "past_due")
    ) && matches!(outcome, Some(Outcome::Applied | Outcome::Stale));
    let gone = if renews {
        gone_account(&mut tx, id).await?
    } else {
        None
    };
    tx.commit().await?;
    let line = format!(
        "{id}: {} (status {}, period ends {})",
        match outcome {
            Some(Outcome::Applied) => "recorded",
            Some(Outcome::Stale) => "already up to date",
            Some(Outcome::UnknownAccount) => "no account in its metadata, not recorded",
            Some(Outcome::UnknownProduct) => "not one of the two plans, not recorded",
            _ => "a status that is never recorded",
        },
        order["status"].as_str().unwrap_or("?"),
        order["currentPeriodEnd"].as_str().unwrap_or("?"),
    );
    let Some(account) = gone else {
        return Ok(line);
    };
    cancel_order(ctx.http, waffo, id)
        .await
        .map(|()| {
            format!(
                "{line}\n{id}: account {account} is deleted or banned; canceled at Waffo, it won't renew."
            )
        })
        .map_err(|e| {
            anyhow::anyhow!(
                "{line}\n{id}: account {account} is deleted or banned but Waffo did not cancel it ({}); run this again",
                e.0.detail
            )
        })
}

/// The account of a recorded subscription, if it is deleted or banned.
async fn gone_account(tx: &mut PgConnection, id: &str) -> Result<Option<i32>, sqlx::Error> {
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

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

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
}
