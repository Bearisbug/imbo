//! API-032 getBilling, API-033 createCheckout, API-034 openPortal,
//! API-035 waffoWebhook, API-071 submitAppStoreTransaction,
//! API-072 appStoreNotification, API-080 buyAlipayPack, API-083
//! alipayNotify, API-084 checkAlipayOrder. Each channel's own endpoints are
//! bound to its adapter here; the rest goes through `pay`.

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::{Extension, Json};
use imbo_proto::ErrorCode;
use imbo_proto::api::{
    AlipayCheckRequest, AlipayCheckResponse, AlipayPackResponse, AppStoreTransactionRequest,
    AppStoreTransactionResponse, AppStoreView, BillingResponse, CheckoutRequest, CheckoutResponse,
    PortalResponse,
};
use tracing::{info, warn};

use super::Body;
use crate::auth::Caller;
use crate::billing;
use crate::config::{Alipay, Apple, Waffo};
use crate::error::{ApiResult, err};
use crate::pay::{self, alipay, apple, waffo as waffo_channel};
use crate::state::St;

fn waffo(st: &St) -> ApiResult<&Waffo> {
    st.cfg
        .waffo
        .as_ref()
        .ok_or_else(|| err(ErrorCode::NotFound, "This server does not take payments."))
}

/// Alipay and the app that sells to this caller (API-032): the web app, or the
/// Android App. The iPhone App keeps to the App Store (ADR-063).
async fn alipay<'a>(st: &'a St, c: &Caller) -> ApiResult<(&'a Alipay, &'a str)> {
    let iphone = match c.app {
        Some(id) => {
            sqlx::query_scalar::<_, String>("SELECT platform FROM app_sessions WHERE id = $1")
                .bind(id)
                .fetch_one(&st.db)
                .await?
                != "android"
        }
        None => false,
    };
    st.cfg
        .alipay
        .as_ref()
        .filter(|_| st.cfg.billing_on() && !iphone)
        .and_then(|a| Some((a, a.seller(c.app.is_some())?)))
        .ok_or_else(|| {
            err(
                ErrorCode::NotFound,
                "This server does not sell time packs here.",
            )
        })
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
    // The paid one that grants access longest, else a gift that does (ADR-060),
    // else the newest.
    let now = chrono::Utc::now();
    let subscription = subs
        .iter()
        .filter(|s| s.grants_until().is_some_and(|g| g > now))
        .max_by_key(|s| (pay::voluntary(&st.cfg, s), s.grants_until()))
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
        alipay: alipay(&st, &c).await.is_ok(),
    }))
}

/// API-080: a cashier page for the web app, an order string for the App.
pub async fn alipay_pack(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
    Body(req): Body<CheckoutRequest>,
) -> ApiResult<Json<AlipayPackResponse>> {
    let (alipay, _) = alipay(&st, &c).await?;
    if req.plan != "monthly" && req.plan != "yearly" {
        return Err(err(
            ErrorCode::InvalidRequest,
            "plan must be monthly or yearly.",
        ));
    }
    let app = c.app.is_some();
    let ch = alipay::Adapter { alipay, app };
    let signed = pay::begin(&st, &ch, c.account_id, &req.plan, ("alipay", 30)).await?;
    Ok(Json(if app {
        AlipayPackResponse {
            purchase_url: None,
            order_string: Some(signed),
        }
    } else {
        AlipayPackResponse {
            purchase_url: Some(signed),
            order_string: None,
        }
    }))
}

/// API-084: the buyer is back from paying and hands in the order number.
pub async fn alipay_check(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
    Body(req): Body<AlipayCheckRequest>,
) -> ApiResult<Json<AlipayCheckResponse>> {
    let (alipay, app_id) = alipay(&st, &c).await?;
    let paid_until = alipay::check(&st, alipay, app_id, c.account_id, &req.out_trade_no).await?;
    Ok(Json(AlipayCheckResponse {
        status: if paid_until.is_some() {
            "granted"
        } else {
            "unpaid"
        }
        .into(),
        paid_until,
    }))
}

/// API-083: only Alipay calls this; the signature is the only credential
/// (ROLE-Alipay). Alipay stops retrying once it reads `success`.
pub async fn alipay_notify(State(st): State<St>, body: Bytes) -> ApiResult<&'static str> {
    let alipay = st
        .cfg
        .alipay
        .as_ref()
        .ok_or_else(|| err(ErrorCode::NotFound, "This server does not sell time packs."))?;
    match alipay::notify(&st, alipay, &body).await {
        Ok(n) => {
            info!(
                event = "billing_event",
                event_id = %n.notify_id,
                r#type = %n.kind,
                subscription_id = %n.out_trade_no,
                outcome = n.outcome.as_str(),
                "alipay notification"
            );
            Ok("success")
        }
        Err(e) => {
            st.note_webhook_error();
            if e.0.code() == Some(ErrorCode::Unauthorized) {
                warn!("alipay notification that does not verify");
            }
            Err(e)
        }
    }
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
    let ch = waffo_channel::Adapter(waffo);
    let checkout_url = pay::begin(&st, &ch, c.account_id, &req.plan, ("checkout", 10)).await?;
    Ok(Json(CheckoutResponse { checkout_url }))
}

pub async fn portal(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
) -> ApiResult<Json<PortalResponse>> {
    waffo(&st)?;
    // A bearer link to the customer page: never log or keep it.
    let portal_url = pay::portal(&st, c.account_id).await?;
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
    if !waffo_channel::signature_ok(&waffo.webhook_key, &body, signature, chrono::Utc::now()) {
        st.note_webhook_error();
        warn!("payment webhook with a bad signature");
        return Err(err(ErrorCode::Unauthorized, "Bad signature."));
    }
    let event: serde_json::Value = serde_json::from_slice(&body).map_err(|_| {
        st.note_webhook_error();
        err(ErrorCode::InvalidRequest, "The body is not JSON.")
    })?;
    match waffo_channel::handle_event(&st, waffo, &event).await {
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
