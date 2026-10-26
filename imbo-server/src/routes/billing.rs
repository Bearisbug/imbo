//! API-032 getBilling, API-033 createCheckout, API-034 openPortal,
//! API-035 creemWebhook.

use std::time::Duration;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::{Extension, Json};
use imbo_proto::ErrorCode;
use imbo_proto::api::{BillingResponse, CheckoutRequest, CheckoutResponse, PortalResponse};
use tracing::{info, warn};

use super::Body;
use crate::auth::Caller;
use crate::billing;
use crate::config::Creem;
use crate::error::{ApiResult, err};
use crate::state::St;

fn creem(st: &St) -> ApiResult<&Creem> {
    st.cfg
        .creem
        .as_ref()
        .ok_or_else(|| err(ErrorCode::NotFound, "This server does not take payments."))
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
        enabled: st.cfg.creem.is_some(),
        mode: st.cfg.creem.as_ref().map(|c| c.mode().to_string()),
        access: access.view(),
        trial_ends_at: st
            .cfg
            .billing_on()
            .then(|| billing::trial_end(&st.cfg, created_at))
            .flatten(),
        subscription,
    }))
}

pub async fn checkout(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
    Body(req): Body<CheckoutRequest>,
) -> ApiResult<Json<CheckoutResponse>> {
    let creem = creem(&st)?;
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
    let checkout_url =
        billing::create_checkout(&st, creem, c.account_id, &email, &req.plan).await?;
    info!(
        event = "checkout_created",
        account_id = c.account_id,
        plan = %req.plan,
        mode = creem.mode(),
        "checkout created"
    );
    Ok(Json(CheckoutResponse { checkout_url }))
}

pub async fn portal(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
) -> ApiResult<Json<PortalResponse>> {
    let creem = creem(&st)?;
    let subs = billing::subscriptions(&st.db, &st.cfg, &[c.account_id]).await?;
    let Some(sub) = subs.first() else {
        return Err(err(
            ErrorCode::NotFound,
            "You have no subscription to manage yet.",
        ));
    };
    let portal_url = billing::portal_link(&st, creem, &sub.customer_id).await?;
    Ok(Json(PortalResponse { portal_url }))
}

/// Only Creem calls this; the signature is the only credential (ROLE-Creem).
pub async fn webhook(
    State(st): State<St>,
    headers: HeaderMap,
    body: Bytes,
) -> ApiResult<StatusCode> {
    let creem = creem(&st)?;
    let signature = headers
        .get("creem-signature")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    if !billing::signature_ok(&creem.webhook_secret, &body, signature) {
        st.note_webhook_error();
        warn!("payment webhook with a bad signature");
        return Err(err(ErrorCode::Unauthorized, "Bad signature."));
    }
    let event: serde_json::Value = serde_json::from_slice(&body).map_err(|_| {
        st.note_webhook_error();
        err(ErrorCode::InvalidRequest, "The body is not JSON.")
    })?;
    match billing::handle_event(&st, creem, &event).await {
        Ok(outcome) => {
            let event_id = event["id"].as_str().unwrap_or_default();
            let kind = event["eventType"].as_str().unwrap_or_default();
            let subscription_id =
                billing::event_subscription(kind, &event["object"]).unwrap_or_default();
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
