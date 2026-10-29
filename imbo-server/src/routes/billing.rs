//! API-032 getBilling, API-033 createCheckout, API-034 openPortal,
//! API-035 waffoWebhook, API-071 submitAppStoreTransaction,
//! API-072 appStoreNotification.

use std::time::Duration;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::{Extension, Json};
use imbo_proto::ErrorCode;
use imbo_proto::api::{
    AppStoreTransactionRequest, AppStoreTransactionResponse, AppStoreView, BillingResponse,
    CheckoutRequest, CheckoutResponse, PortalResponse,
};
use tracing::{info, warn};

use super::Body;
use crate::auth::Caller;
use crate::config::{Apple, Waffo};
use crate::error::{ApiResult, err};
use crate::state::St;
use crate::{apple, billing};

fn waffo(st: &St) -> ApiResult<&Waffo> {
    st.cfg
        .waffo
        .as_ref()
        .ok_or_else(|| err(ErrorCode::NotFound, "This server does not take payments."))
}

fn app_store(st: &St) -> ApiResult<&Apple> {
    st.cfg.apple.as_ref().ok_or_else(|| {
        err(
            ErrorCode::NotFound,
            "This server does not sell App Store subscriptions.",
        )
    })
}

pub async fn get(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
) -> ApiResult<Json<BillingResponse>> {
    c.require_person()?;
    let access = billing::access(&st.db, &st.cfg, c.account_id).await?;
    let created_at: chrono::DateTime<chrono::Utc> =
        sqlx::query_scalar("SELECT created_at FROM accounts WHERE id = $1")
            .bind(c.account_id)
            .fetch_one(&st.db)
            .await?;
    let subs = billing::subscriptions(&st.db, &st.cfg, &[c.account_id]).await?;
    // The one that grants access longest, else the newest.
    let subscription = subs
        .iter()
        .filter(|s| s.grants_until().is_some_and(|g| g > chrono::Utc::now()))
        .max_by_key(|s| s.grants_until())
        .or(subs.first())
        .map(billing::SubRow::view);
    Ok(Json(BillingResponse {
        enabled: st.cfg.waffo.is_some(),
        mode: st.cfg.waffo.as_ref().map(|w| w.mode().to_string()),
        access: access.view(),
        trial_ends_at: st
            .cfg
            .billing_on()
            .then(|| billing::trial_end(&st.cfg, created_at))
            .flatten(),
        subscription,
        app_store: st.cfg.apple.as_ref().map(|a| AppStoreView {
            product_monthly: a.product_monthly.clone(),
            product_yearly: a.product_yearly.clone(),
            account_token: apple::account_token(c.account_id).to_string(),
        }),
    }))
}

pub async fn checkout(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
    Body(req): Body<CheckoutRequest>,
) -> ApiResult<Json<CheckoutResponse>> {
    let waffo = waffo(&st)?;
    if req.plan != "monthly" && req.plan != "yearly" {
        return Err(err(
            ErrorCode::InvalidRequest,
            "plan must be monthly or yearly.",
        ));
    }
    if let Err(wait) = st.limiter.hit(
        &format!("checkout:{}", c.account_id),
        10,
        Duration::from_secs(3600),
    ) {
        return Err(err(
            ErrorCode::RateLimited,
            "Too many checkouts in the last hour. Try again later.",
        )
        .retry_after(wait));
    }
    let now = chrono::Utc::now();
    let subs = billing::subscriptions(&st.db, &st.cfg, &[c.account_id]).await?;
    if subs
        .iter()
        .any(|s| s.grants_until().is_some_and(|g| g > now))
    {
        return Err(err(
            ErrorCode::AlreadySubscribed,
            "You already have a subscription. Change it under Manage subscription.",
        ));
    }
    let email: String = sqlx::query_scalar("SELECT email FROM accounts WHERE id = $1")
        .bind(c.account_id)
        .fetch_one(&st.db)
        .await?;
    // ADR-019: still in the trial, the first charge comes 30 days later.
    let with_trial = matches!(
        billing::access(&st.db, &st.cfg, c.account_id).await?,
        billing::Access::Trial(_)
    );
    let checkout_url =
        billing::create_checkout(&st, waffo, c.account_id, &email, &req.plan, with_trial).await?;
    info!(
        event = "checkout_created",
        account_id = c.account_id,
        plan = %req.plan,
        mode = waffo.mode(),
        "checkout created"
    );
    Ok(Json(CheckoutResponse { checkout_url }))
}

pub async fn portal(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
) -> ApiResult<Json<PortalResponse>> {
    let waffo = waffo(&st)?;
    let subs = billing::subscriptions(&st.db, &st.cfg, &[c.account_id]).await?;
    if !subs.iter().any(|s| s.provider == "waffo") {
        return Err(err(
            ErrorCode::NotFound,
            "You have no subscription to manage here. App Store subscriptions are managed in Settings on your iPhone.",
        ));
    }
    // A bearer link to the customer page: never log or keep it.
    let portal_url = billing::portal_link(&st, waffo, c.account_id).await?;
    Ok(Json(PortalResponse { portal_url }))
}

/// API-071: only the App hands in transactions.
pub async fn app_store_transaction(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
    Body(req): Body<AppStoreTransactionRequest>,
) -> ApiResult<Json<AppStoreTransactionResponse>> {
    if c.app.is_none() {
        return Err(err(ErrorCode::Forbidden, "Only the Imbo App can do this."));
    }
    let apple = app_store(&st)?;
    let row = apple::submit(&st, apple, c.account_id, &req.signed_transaction).await?;
    Ok(Json(AppStoreTransactionResponse {
        subscription: row.view(),
    }))
}

/// API-072: only Apple calls this; the signature is the only credential (ROLE-Apple).
pub async fn app_store_notification(State(st): State<St>, body: Bytes) -> ApiResult<StatusCode> {
    let apple = app_store(&st)?;
    match apple::handle_notification(&st, apple, &body).await {
        Ok(n) => {
            info!(
                event = "billing_event",
                event_id = %n.uuid,
                r#type = %n.kind,
                subscription_id = %n.subscription_id,
                outcome = n.outcome.as_str(),
                "app store notification"
            );
            Ok(StatusCode::OK)
        }
        Err(e) => {
            st.note_webhook_error();
            if e.0.code() == Some(ErrorCode::Unauthorized) {
                warn!("app store notification that does not verify");
            }
            Err(e)
        }
    }
}

/// Only Waffo calls this; the signature is the only credential (ROLE-Waffo).
pub async fn webhook(
    State(st): State<St>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<StatusCode> {
    let waffo = waffo(&st)?;
    let signature = headers
        .get("x-waffo-signature")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if !billing::signature_ok(&waffo.webhook_key, &body, signature, chrono::Utc::now()) {
        st.note_webhook_error();
        warn!("payment webhook with a bad signature");
        return Err(err(ErrorCode::Unauthorized, "Bad signature."));
    }
    let event: serde_json::Value = serde_json::from_slice(&body).map_err(|_| {
        st.note_webhook_error();
        err(ErrorCode::InvalidRequest, "The body is not JSON.")
    })?;
    match billing::handle_event(&st, waffo, &event).await {
        Ok(outcome) => {
            let event_id = event["eventId"].as_str().unwrap_or_default();
            let kind = event["eventType"].as_str().unwrap_or_default();
            let subscription_id = event["data"]["orderId"].as_str().unwrap_or_default();
            let outcome = outcome.as_str();
            info!(
                event = "billing_event",
                event_id,
                r#type = kind,
                subscription_id,
                outcome,
                "payment webhook"
            );
            Ok(StatusCode::OK)
        }
        Err(e) => {
            st.note_webhook_error();
            Err(e)
        }
    }
}
