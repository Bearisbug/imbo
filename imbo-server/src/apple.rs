//! App Store subscriptions (ADR-043): transactions the App hands in
//! (API-071), Apple's notifications (API-072), and asking Apple for a
//! subscription's current state (API-052, `imbo-server billing pull`).

use app_store_server_library::models::app_store_environment::Environment;
use app_store_server_library::models::auto_renew_status::AutoRenewStatus;
use app_store_server_library::models::jws_transaction_decoded_payload::JWSTransactionDecodedPayload;
use app_store_server_library::models::response_body_v2_decoded_payload::ResponseBodyV2DecodedPayload;
use app_store_server_library::models::status::Status;
use app_store_server_library::models::status_response::StatusResponse;
use app_store_server_library::models::subtype::Subtype;
use app_store_server_library::signed_data_verifier::SignedDataVerifier;
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, Utc};
use imbo_proto::ErrorCode;
use ring::rand::SystemRandom;
use ring::signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair};
use serde_json::{Value, json};
use sqlx::{PgConnection, PgPool};
use tracing::{info, warn};
use uuid::Uuid;

use crate::billing::{self, Outcome, SubRow, provider_failed};
use crate::config::{Apple, Config};
use crate::error::{ApiError, ApiResult, err};
use crate::state::St;

/// API-032: the appAccountToken the App passes to Apple for `account`.
pub fn account_token(account: i32) -> Uuid {
    let namespace = Uuid::new_v5(&Uuid::NAMESPACE_DNS, b"imbo.dev");
    Uuid::new_v5(&namespace, account.to_string().as_bytes())
}

/// The account whose token this is, deleted and banned ones included.
async fn account_of_token(tx: &mut PgConnection, token: Uuid) -> Result<Option<i32>, sqlx::Error> {
    let ids: Vec<i32> = sqlx::query_scalar("SELECT id FROM accounts")
        .fetch_all(&mut *tx)
        .await?;
    Ok(ids.into_iter().find(|id| account_token(*id) == token))
}

async fn is_review(tx: &mut PgConnection, cfg: &Config, account: i32) -> Result<bool, sqlx::Error> {
    let email: Option<String> = sqlx::query_scalar("SELECT email FROM accounts WHERE id = $1")
        .bind(account)
        .fetch_optional(&mut *tx)
        .await?;
    Ok(email.is_some_and(|e| cfg.review_code(&e).is_some()))
}

/// The account a recorded subscription is written to: its own, or the
/// account of `token` when that is another account and the recorded
/// subscription no longer grants access (API-072). `true` when it moves.
async fn holder(
    tx: &mut PgConnection,
    recorded: &SubRow,
    token: Option<Uuid>,
) -> Result<(i32, bool), sqlx::Error> {
    let keep = (recorded.account_id, false);
    if recorded.grants_until().is_some_and(|g| g > Utc::now()) {
        return Ok(keep);
    }
    let Some(token) = token else {
        return Ok(keep);
    };
    Ok(match account_of_token(tx, token).await? {
        Some(a) if a != recorded.account_id => (a, true),
        _ => keep,
    })
}

/// RULE-007, ADR-043: the mode data from `env` is recorded in, if this
/// server takes it. Live takes Production, and Sandbox from the review
/// account; test takes Sandbox, and Xcode's local tests under IMBO_DEV.
fn accepted_mode(cfg: &Config, env: &Environment, review: bool) -> Option<&'static str> {
    let live = cfg.waffo.as_ref()?.live;
    match env {
        Environment::Production if live => Some("live"),
        Environment::Sandbox if !live || review => Some("test"),
        Environment::Xcode if !live && cfg.dev => Some("test"),
        _ => None,
    }
}

/// Where a subscription recorded in `mode` lives at Apple.
fn env_of(mode: &str) -> Environment {
    if mode == "live" {
        Environment::Production
    } else {
        Environment::Sandbox
    }
}

/// The environment a JWS's payload claims, read before verifying it;
/// notifications keep it under `data`.
fn claimed_env(jws: &str) -> Option<Environment> {
    let payload = URL_SAFE_NO_PAD.decode(jws.split('.').nth(1)?).ok()?;
    let v: Value = serde_json::from_slice(&payload).ok()?;
    let env = if v["data"].is_object() {
        &v["data"]["environment"]
    } else {
        &v["environment"]
    };
    serde_json::from_value(env.clone()).ok()
}

/// Checks signatures against the root certificate, and the bundle ID, the
/// App's Apple ID (production notifications) and the environment.
fn verifier(apple: &Apple, env: Environment) -> SignedDataVerifier {
    SignedDataVerifier::new(
        vec![apple.root_cert.clone()],
        env,
        apple.bundle_id.clone(),
        Some(apple.app_id),
        true,
    )
    .expect("the App's Apple ID is set")
}

// ---- asking Apple ----

/// Apple's current state of one subscription.
pub struct Current {
    status: Status,
    auto_renew: bool,
    /// The latest transaction.
    tx: JWSTransactionDecodedPayload,
}

impl Current {
    /// API-052: Apple will charge again.
    fn renews(&self) -> bool {
        self.auto_renew
            && matches!(
                self.status,
                Status::Active | Status::BillingRetry | Status::BillingGracePeriod
            )
    }

    /// API-071's table: the status to record, `None` to keep the recorded
    /// one; and whether it is refunded or revoked.
    fn recorded(&self) -> (Option<&'static str>, bool) {
        let status = match self.status {
            Status::Active if self.auto_renew => Some("active"),
            Status::Active => Some("scheduled_cancel"),
            Status::BillingRetry | Status::BillingGracePeriod => Some("past_due"),
            Status::Expired => Some("canceled"),
            _ => None,
        };
        let revoked = matches!(self.status, Status::Revoked) || self.tx.revocation_date.is_some();
        (status, revoked)
    }
}

pub enum AskError {
    /// Apple has no such subscription in that environment.
    NotFound,
    /// No answer, an error, or data that doesn't verify.
    Failed,
}

/// The App Store Server API's bearer token: ES256 with the In-App Purchase key.
fn api_token(apple: &Apple) -> Option<String> {
    let now = Utc::now().timestamp();
    let part = |v: Value| URL_SAFE_NO_PAD.encode(v.to_string());
    let input = format!(
        "{}.{}",
        part(json!({ "alg": "ES256", "kid": apple.key_id, "typ": "JWT" })),
        part(json!({
            "iss": apple.issuer_id,
            "iat": now,
            "exp": now + 300,
            "aud": "appstoreconnect-v1",
            "bid": apple.bundle_id,
        })),
    );
    let rng = SystemRandom::new();
    let key = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &apple.key, &rng).ok()?;
    let sig = key.sign(&rng, input.as_bytes()).ok()?;
    Some(format!("{input}.{}", URL_SAFE_NO_PAD.encode(sig.as_ref())))
}

/// Get All Subscription Statuses for one original transaction ID, verified.
/// `http` times out after 10 seconds.
pub async fn ask(
    http: &reqwest::Client,
    apple: &Apple,
    env: &Environment,
    original: &str,
) -> Result<Current, AskError> {
    if original.is_empty() || !original.bytes().all(|b| b.is_ascii_digit()) {
        return Err(AskError::NotFound);
    }
    let base = match (&apple.api_url, env) {
        (Some(url), Environment::Production) => format!("{url}/production"),
        (Some(url), _) => format!("{url}/sandbox"),
        (None, Environment::Production) => "https://api.storekit.apple.com".into(),
        (None, _) => "https://api.storekit-sandbox.apple.com".into(),
    };
    let token = api_token(apple).ok_or_else(|| {
        warn!("signing an App Store request failed");
        AskError::Failed
    })?;
    let res = http
        .get(format!("{base}/inApps/v1/subscriptions/{original}"))
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| {
            warn!(error = %e, subscription_id = original, "app store request failed");
            AskError::Failed
        })?;
    let status = res.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        return Err(AskError::NotFound);
    }
    if !status.is_success() {
        warn!(%status, subscription_id = original, "app store refused");
        return Err(AskError::Failed);
    }
    let body: StatusResponse = res.json().await.map_err(|e| {
        warn!(error = %e, subscription_id = original, "app store answer unreadable");
        AskError::Failed
    })?;
    let item = body
        .data
        .into_iter()
        .flatten()
        .flat_map(|g| g.last_transactions.into_iter().flatten())
        .find(|t| t.original_transaction_id.as_deref() == Some(original))
        .ok_or(AskError::NotFound)?;
    let v = verifier(apple, env.clone());
    let unverified = |what: &str| {
        warn!(
            subscription_id = original,
            what, "app store answer does not verify"
        );
        AskError::Failed
    };
    let tx = item
        .signed_transaction_info
        .as_deref()
        .and_then(|s| v.verify_and_decode_signed_transaction(s).ok())
        .ok_or_else(|| unverified("transaction"))?;
    let renewal = item
        .signed_renewal_info
        .as_deref()
        .and_then(|s| v.verify_and_decode_renewal_info(s).ok())
        .ok_or_else(|| unverified("renewal info"))?;
    Ok(Current {
        status: item.status.ok_or_else(|| unverified("status"))?,
        auto_renew: renewal.auto_renew_status == Some(AutoRenewStatus::On),
        tx,
    })
}

// ---- recording ----

/// One write to an App Store subscription.
struct Change<'a> {
    id: &'a str,
    mode: &'a str,
    account: i32,
    plan: &'a str,
    /// `None` keeps the recorded status; a new record is `canceled`.
    status: Option<&'a str>,
    paid_until: DateTime<Utc>,
    /// Keep the recorded plan and paid-through date.
    keep: bool,
    /// `Some(true)` revokes from now on, `Some(false)` clears a revocation
    /// recorded before `updated`.
    revoked: Option<bool>,
    /// Older than what is recorded changes nothing; equal applies.
    updated: DateTime<Utc>,
    /// The recorded subscription moves to `account`.
    moving: bool,
}

async fn upsert(tx: &mut PgConnection, c: Change<'_>) -> Result<Outcome, sqlx::Error> {
    let changed = sqlx::query(
        "INSERT INTO subscriptions
           (id, provider, mode, account_id, plan, status, paid_until, revoked_at, provider_updated_at)
         VALUES ($1, 'apple', $2, $3, $4, coalesce($5::text, 'canceled'), $6,
                 CASE WHEN $8::boolean THEN now() END, $9)
         ON CONFLICT (id) DO UPDATE SET
           account_id = EXCLUDED.account_id,
           plan = CASE WHEN $7 THEN subscriptions.plan ELSE EXCLUDED.plan END,
           status = coalesce($5::text, subscriptions.status),
           paid_until = CASE WHEN $7 THEN subscriptions.paid_until ELSE EXCLUDED.paid_until END,
           revoked_at = CASE WHEN $8::boolean IS NULL THEN subscriptions.revoked_at
                             WHEN $8::boolean THEN coalesce(subscriptions.revoked_at, now())
                             WHEN subscriptions.revoked_at < EXCLUDED.provider_updated_at THEN NULL
                             ELSE subscriptions.revoked_at END,
           provider_updated_at = EXCLUDED.provider_updated_at
         WHERE (subscriptions.account_id = EXCLUDED.account_id OR $10)
           AND subscriptions.provider_updated_at <= EXCLUDED.provider_updated_at",
    )
    .bind(c.id)
    .bind(c.mode)
    .bind(c.account)
    .bind(c.plan)
    .bind(c.status)
    .bind(c.paid_until)
    .bind(c.keep)
    .bind(c.revoked)
    .bind(c.updated)
    .bind(c.moving)
    .execute(&mut *tx)
    .await?;
    Ok(if changed.rows_affected() == 0 {
        Outcome::Stale
    } else {
        Outcome::Applied
    })
}

/// Record Apple's current state as of `asked`, the time it was asked.
#[allow(clippy::too_many_arguments)]
async fn record_current(
    tx: &mut PgConnection,
    id: &str,
    mode: &str,
    account: i32,
    moving: bool,
    plan: &str,
    cur: &Current,
    asked: DateTime<Utc>,
) -> Result<Outcome, sqlx::Error> {
    let (status, revoked) = cur.recorded();
    upsert(
        tx,
        Change {
            id,
            mode,
            account,
            plan,
            status,
            paid_until: cur.tx.expires_date.unwrap_or(asked),
            keep: false,
            revoked: Some(revoked),
            updated: asked,
            moving,
        },
    )
    .await
}

async fn row(db: &PgPool, id: &str) -> Result<Option<SubRow>, sqlx::Error> {
    sqlx::query_as(
        "SELECT id, account_id, provider, mode, plan, status, paid_until, revoked_at
         FROM subscriptions WHERE id = $1 AND provider = 'apple'",
    )
    .bind(id)
    .fetch_optional(db)
    .await
}

/// Whether `me` may write subscription `id`: `Ok(true)` when it is another
/// account's and moves to `me`, 409 when it stays theirs. Locks the row in `tx`.
async fn claim(tx: &mut PgConnection, id: &str, me: i32, token: Option<Uuid>) -> ApiResult<bool> {
    let recorded: Option<SubRow> = sqlx::query_as(
        "SELECT id, account_id, provider, mode, plan, status, paid_until, revoked_at
         FROM subscriptions WHERE id = $1 AND provider = 'apple' FOR UPDATE",
    )
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    match recorded {
        Some(r) if r.account_id != me => match holder(tx, &r, token).await? {
            (a, true) if a == me => Ok(true),
            _ => Err(other_account()),
        },
        _ => Ok(false),
    }
}

fn other_account() -> ApiError {
    err(
        ErrorCode::OtherAccount,
        "This App Store subscription belongs to another Imbo account. Sign in to that account to restore it.",
    )
}

fn invalid(detail: &str) -> ApiError {
    err(ErrorCode::InvalidRequest, detail)
}

// ---- API-071 ----

/// Verify a StoreKit 2 transaction of `me`'s and record its subscription as
/// Apple has it now; Xcode's local tests are recorded as they are.
pub async fn submit(st: &St, apple: &Apple, me: i32, jws: &str) -> ApiResult<SubRow> {
    let mut conn = st.db.acquire().await?;
    let review = is_review(&mut conn, &st.cfg, me).await?;
    drop(conn);
    let env = claimed_env(jws).ok_or_else(|| invalid("This is not an App Store transaction."))?;
    let mode = accepted_mode(&st.cfg, &env, review).ok_or_else(|| {
        invalid("This transaction comes from an App Store environment this server doesn't take.")
    })?;
    let t = verifier(apple, env.clone())
        .verify_and_decode_signed_transaction(jws)
        .map_err(|_| invalid("The transaction's signature or app doesn't check out."))?;
    let plan = t
        .product_id
        .as_deref()
        .and_then(|p| apple.plan(p))
        .ok_or_else(|| invalid("The transaction is not for an Imbo subscription."))?;
    // Without a token it is taken as `me`'s, unless another account has it.
    if t.app_account_token
        .is_some_and(|tok| tok != account_token(me))
    {
        return Err(other_account());
    }
    let id = t
        .original_transaction_id
        .clone()
        .filter(|id| !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit()))
        .ok_or_else(|| invalid("The transaction has no original transaction ID."))?;
    // Checked before asking Apple as if the latest transaction were `me`'s,
    // and again under the row lock below with Apple's latest one.
    claim(
        &mut *st.db.acquire().await?,
        &id,
        me,
        Some(account_token(me)),
    )
    .await?;
    let now = Utc::now();
    let outcome = if env == Environment::Xcode {
        let paid_until = t.expires_date.unwrap_or(now);
        let status = if paid_until > now {
            "active"
        } else {
            "canceled"
        };
        let mut tx = st.db.begin().await?;
        let moving = claim(&mut tx, &id, me, t.app_account_token).await?;
        let o = upsert(
            &mut tx,
            Change {
                id: &id,
                mode,
                account: me,
                plan,
                status: Some(status),
                paid_until,
                keep: false,
                revoked: Some(t.revocation_date.is_some()),
                updated: now,
                moving,
            },
        )
        .await?;
        tx.commit().await?;
        o
    } else {
        let cur = ask(&st.http, apple, &env, &id)
            .await
            .map_err(|_| provider_failed())?;
        let plan = cur
            .tx
            .product_id
            .as_deref()
            .and_then(|p| apple.plan(p))
            .unwrap_or(plan);
        let mut tx = st.db.begin().await?;
        let moving = claim(&mut tx, &id, me, cur.tx.app_account_token).await?;
        let o = record_current(&mut tx, &id, mode, me, moving, plan, &cur, now).await?;
        tx.commit().await?;
        o
    };
    let recorded = row(&st.db, &id)
        .await?
        .filter(|r| r.account_id == me)
        .ok_or_else(other_account)?;
    info!(
        event = "app_store_transaction",
        account_id = me,
        subscription_id = %id,
        status = %recorded.status,
        outcome = outcome.as_str(),
        "app store transaction"
    );
    if outcome == Outcome::Applied
        && let Err(e) = billing::warn_if_duplicate(st, &id).await
    {
        warn!(error = %e, "duplicate subscription check failed");
    }
    Ok(recorded)
}

// ---- API-072 ----

/// What a notification was, for the log.
pub struct Notice {
    pub uuid: String,
    pub kind: String,
    pub subscription_id: String,
    pub outcome: Outcome,
}

fn unauthorized() -> ApiError {
    err(ErrorCode::Unauthorized, "Bad signature.")
}

/// Verify one App Store Server Notification V2, then store it and apply it
/// in one transaction (INV-015).
pub async fn handle_notification(st: &St, apple: &Apple, body: &[u8]) -> ApiResult<Notice> {
    let signed: Value = serde_json::from_slice(body).map_err(|_| unauthorized())?;
    let signed = signed["signedPayload"].as_str().ok_or_else(unauthorized)?;
    let env = claimed_env(signed)
        .filter(|e| matches!(e, Environment::Sandbox | Environment::Production))
        .ok_or_else(unauthorized)?;
    let v = verifier(apple, env.clone());
    let n: ResponseBodyV2DecodedPayload = v
        .verify_and_decode_notification(signed)
        .map_err(|_| unauthorized())?;
    let t = match n
        .data
        .as_ref()
        .and_then(|d| d.signed_transaction_info.as_deref())
    {
        Some(s) => Some(
            v.verify_and_decode_signed_transaction(s)
                .map_err(|_| unauthorized())?,
        ),
        None => None,
    };
    let uuid = n.notification_uuid.clone().ok_or_else(unauthorized)?;
    let kind = n
        .notification_type
        .as_ref()
        .map(|k| k.raw_value().to_string())
        .unwrap_or_default();
    let original = t
        .as_ref()
        .and_then(|t| t.original_transaction_id.clone())
        .unwrap_or_default();
    let signed_at = n.signed_date.unwrap_or_else(Utc::now);

    let mut tx = st.db.begin().await?;
    let inserted = sqlx::query(
        "INSERT INTO billing_events (id, type, subscription_id, payload) VALUES ($1, $2, $3, $4)
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(format!("apple:{uuid}"))
    .bind(&kind)
    .bind(Some(&original).filter(|o| !o.is_empty()))
    .bind(sqlx::types::Json(&n))
    .execute(&mut *tx)
    .await?;
    let outcome = if inserted.rows_affected() == 0 {
        Outcome::Duplicate
    } else {
        apply(
            &mut tx,
            st,
            apple,
            &n,
            t.as_ref(),
            &env,
            &original,
            signed_at,
        )
        .await?
    };
    tx.commit().await?;
    if outcome == Outcome::Applied
        && matches!(kind.as_str(), "SUBSCRIBED" | "DID_RENEW")
        && let Err(e) = billing::warn_if_duplicate(st, &original).await
    {
        warn!(error = %e, "duplicate subscription check failed");
    }
    Ok(Notice {
        uuid,
        kind,
        subscription_id: original,
        outcome,
    })
}

/// API-072's table.
#[allow(clippy::too_many_arguments)]
async fn apply(
    tx: &mut PgConnection,
    st: &St,
    apple: &Apple,
    n: &ResponseBodyV2DecodedPayload,
    t: Option<&JWSTransactionDecodedPayload>,
    env: &Environment,
    original: &str,
    signed_at: DateTime<Utc>,
) -> Result<Outcome, sqlx::Error> {
    let kind = n.notification_type.as_ref().map(|k| k.raw_value());
    // The status to record and whether to keep the plan and paid-through
    // date; `None` for a refund or its reversal.
    let set = match (kind, n.subtype.as_ref()) {
        (Some("SUBSCRIBED" | "DID_RENEW"), _) => Some(("active", false)),
        (Some("DID_FAIL_TO_RENEW"), _) => Some(("past_due", true)),
        (Some("DID_CHANGE_RENEWAL_STATUS"), Some(Subtype::AutoRenewDisabled)) => {
            Some(("scheduled_cancel", true))
        }
        (Some("DID_CHANGE_RENEWAL_STATUS"), Some(Subtype::AutoRenewEnabled)) => {
            Some(("active", true))
        }
        // Apple goes on retrying the charge after the grace period, as ask() sees it.
        (Some("GRACE_PERIOD_EXPIRED"), _) => Some(("past_due", true)),
        (Some("EXPIRED"), _) => Some(("canceled", true)),
        (Some("REFUND" | "REFUND_REVERSED"), _) => None,
        _ => return Ok(Outcome::Stored),
    };
    let Some(t) = t.filter(|_| !original.is_empty()) else {
        return Ok(Outcome::UnknownAccount);
    };
    // The account: the recorded subscription's, or the token's when it moves; else the token's.
    let recorded: Option<SubRow> = sqlx::query_as(
        "SELECT id, account_id, provider, mode, plan, status, paid_until, revoked_at
         FROM subscriptions WHERE id = $1 AND provider = 'apple' FOR UPDATE",
    )
    .bind(original)
    .fetch_optional(&mut *tx)
    .await?;
    let (account, moving) = match (&recorded, t.app_account_token) {
        (Some(r), token) => holder(tx, r, token).await?,
        (None, Some(token)) => match account_of_token(tx, token).await? {
            Some(a) => (a, false),
            None => return Ok(Outcome::UnknownAccount),
        },
        (None, None) => return Ok(Outcome::UnknownAccount),
    };
    let review = is_review(tx, &st.cfg, account).await?;
    let Some(mode) = accepted_mode(&st.cfg, env, review) else {
        return Ok(Outcome::WrongMode);
    };
    let Some((status, keep)) = set else {
        // A refund takes effect whatever was recorded since (RULE-007).
        if recorded.is_none() {
            return Ok(Outcome::UnknownAccount);
        }
        // Older than a stored notification of the other kind: store only.
        let other = if kind == Some("REFUND") {
            "REFUND_REVERSED"
        } else {
            "REFUND"
        };
        let newer: bool = sqlx::query_scalar(
            "SELECT EXISTS (SELECT 1 FROM billing_events WHERE subscription_id = $1 AND type = $2
               AND (payload->>'signedDate')::bigint > $3)",
        )
        .bind(original)
        .bind(other)
        .bind(signed_at.timestamp_millis())
        .fetch_one(&mut *tx)
        .await?;
        if newer {
            return Ok(Outcome::Stale);
        }
        if kind == Some("REFUND") {
            billing::revoke(tx, original).await?;
        } else {
            sqlx::query("UPDATE subscriptions SET revoked_at = NULL WHERE id = $1")
                .bind(original)
                .execute(&mut *tx)
                .await?;
        }
        return Ok(Outcome::Applied);
    };
    let Some(plan) = t.product_id.as_deref().and_then(|p| apple.plan(p)) else {
        return Ok(Outcome::UnknownProduct);
    };
    upsert(
        tx,
        Change {
            id: original,
            mode,
            account,
            plan,
            status: Some(status),
            paid_until: t.expires_date.unwrap_or(signed_at),
            keep,
            // A renewal, a new subscription or a move doesn't carry an earlier refund.
            revoked: (moving || matches!(kind, Some("SUBSCRIBED" | "DID_RENEW"))).then_some(false),
            updated: signed_at,
            moving,
        },
    )
    .await
}

// ---- API-052, API-066, billing pull ----

/// The account's App Store subscriptions that still renew, as RULE-007
/// counts subscriptions; a refunded one doesn't.
pub async fn renewing(db: &PgPool, cfg: &Config, account: i32) -> Result<Vec<SubRow>, sqlx::Error> {
    Ok(billing::subscriptions(db, cfg, &[account])
        .await?
        .into_iter()
        .filter(|s| {
            s.provider == "apple"
                && s.revoked_at.is_none()
                && matches!(s.status.as_str(), "active" | "past_due")
        })
        .collect())
}

/// API-052: before deleting, ask Apple about each App Store subscription
/// that still renews. Apple still charging: 409; no answer: 502; turned off:
/// record it as Apple has it and go on.
pub async fn check_before_delete(st: &St, account: i32) -> ApiResult<()> {
    let Some(apple) = &st.cfg.apple else {
        return Ok(());
    };
    for s in renewing(&st.db, &st.cfg, account).await? {
        let asked = Utc::now();
        let cur = ask(&st.http, apple, &env_of(&s.mode), &s.id)
            .await
            .map_err(|_| provider_failed())?;
        if cur.renews() {
            return Err(err(
                ErrorCode::AppStoreRenewing,
                "Cancel your App Store subscription in Settings on your iPhone first; deleting doesn't stop Apple charging you.",
            ));
        }
        let plan = cur
            .tx
            .product_id
            .as_deref()
            .and_then(|p| apple.plan(p))
            .unwrap_or(s.plan.as_str());
        let mut tx = st.db.begin().await?;
        record_current(&mut tx, &s.id, &s.mode, account, false, plan, &cur, asked).await?;
        tx.commit().await?;
    }
    Ok(())
}

/// `imbo-server billing pull <original transaction ID>` (RB-008): record
/// what Apple has now. The account is the recorded one, or the token's when
/// it moves (API-072); else the token's.
pub async fn pull(
    db: &PgPool,
    cfg: &Config,
    apple: &Apple,
    http: &reqwest::Client,
    id: &str,
) -> anyhow::Result<String> {
    let recorded = row(db, id).await?;
    let live = cfg.waffo.as_ref().is_some_and(|w| w.live);
    let mut env = match &recorded {
        Some(r) => env_of(&r.mode),
        None if live => Environment::Production,
        None => Environment::Sandbox,
    };
    let asked = Utc::now();
    let mut answer = ask(http, apple, &env, id).await;
    // Not recorded yet: it may be the review account's sandbox subscription.
    if recorded.is_none() && live && matches!(answer, Err(AskError::NotFound)) {
        env = Environment::Sandbox;
        answer = ask(http, apple, &env, id).await;
    }
    let cur = match answer {
        Ok(c) => c,
        Err(AskError::NotFound) => anyhow::bail!(
            "{id}: Apple has no subscription with this original transaction ID in {env:?}; nothing recorded"
        ),
        Err(AskError::Failed) => {
            anyhow::bail!("{id}: Apple did not answer; nothing recorded, run this again")
        }
    };
    let mut tx = db.begin().await?;
    let account = match (&recorded, cur.tx.app_account_token) {
        (Some(r), token) => Some(holder(&mut tx, r, token).await?),
        (None, Some(token)) => account_of_token(&mut tx, token).await?.map(|a| (a, false)),
        (None, None) => None,
    };
    let Some((account, moving)) = account else {
        anyhow::bail!("{id}: no Imbo account has its appAccountToken; not recorded");
    };
    let mode = match &recorded {
        Some(r) => r.mode.clone(),
        None => match accepted_mode(cfg, &env, is_review(&mut tx, cfg, account).await?) {
            Some(m) => m.to_string(),
            None => anyhow::bail!(
                "{id}: a {env:?} subscription this server doesn't take (only the review account's sandbox one); not recorded"
            ),
        },
    };
    let Some(plan) = cur.tx.product_id.as_deref().and_then(|p| apple.plan(p)) else {
        anyhow::bail!("{id}: not one of the two Imbo products; not recorded");
    };
    record_current(&mut tx, id, &mode, account, moving, plan, &cur, asked).await?;
    tx.commit().await?;
    let r = row(db, id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("{id}: not recorded"))?;
    Ok(format!(
        "{id}: recorded for account {} (status {}, paid until {})",
        r.account_id,
        r.status,
        r.paid_until.to_rfc3339()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Waffo;

    fn cfg(live: bool, dev: bool) -> Config {
        Config {
            waffo: Some(Waffo {
                merchant_id: "MER_x".into(),
                store_id: "STO_x".into(),
                key: vec![],
                live,
                product_monthly: "PROD_m".into(),
                product_yearly: "PROD_y".into(),
                webhook_key: vec![],
            }),
            dev,
            ..Config::default()
        }
    }

    #[test]
    fn environments_each_mode_takes() {
        use Environment::*;
        let test = cfg(false, false);
        assert_eq!(accepted_mode(&test, &Sandbox, false), Some("test"));
        assert_eq!(accepted_mode(&test, &Production, false), None);
        assert_eq!(accepted_mode(&test, &Xcode, false), None);
        assert_eq!(
            accepted_mode(&cfg(false, true), &Xcode, false),
            Some("test")
        );
        let live = cfg(true, true);
        assert_eq!(accepted_mode(&live, &Production, false), Some("live"));
        assert_eq!(accepted_mode(&live, &Sandbox, false), None);
        assert_eq!(accepted_mode(&live, &Sandbox, true), Some("test"));
        assert_eq!(accepted_mode(&live, &Xcode, true), None);
        assert_eq!(accepted_mode(&live, &LocalTesting, true), None);
    }

    #[test]
    fn account_token_is_uuid5_of_the_decimal_id() {
        // uuid.uuid5(uuid.uuid5(uuid.NAMESPACE_DNS, "imbo.dev"), "12345678")
        assert_eq!(
            account_token(12345678).to_string(),
            "a682075a-9cbc-5239-8764-018ec32882f9"
        );
    }
}
