mod admin;
mod app;
mod auth;
mod avatars;
mod billing;
mod contacts;
mod conversations;
mod files;
mod groups;
mod health;
mod me;
mod messages;
mod safety;

use axum::Router;
use axum::extract::rejection::JsonRejection;
use axum::extract::{FromRequest, Request};
use axum::middleware;
use axum::routing::{delete, get, post, put};
use imbo_proto::ErrorCode;
use serde::de::DeserializeOwned;

use crate::error::{ApiError, err};
use crate::state::St;
use crate::{dev, ws};

pub fn router(st: St) -> Router {
    let authed = Router::new()
        .route("/v1/auth/logout", post(me::logout))
        .route("/v1/me", get(me::get_me).patch(me::rename))
        .route("/v1/devices", get(me::list_devices))
        .route("/v1/devices/{id}", delete(me::revoke_device))
        .route("/v1/contacts/requests", post(contacts::request_contact))
        .route("/v1/contacts", get(contacts::list_contacts))
        .route(
            "/v1/contacts/{account_id}",
            delete(contacts::remove_contact),
        )
        .route("/v1/requests", get(contacts::list_requests))
        .route("/v1/requests/{id}/accept", post(contacts::accept))
        .route("/v1/requests/{id}/decline", post(contacts::decline))
        .route("/v1/agents", get(me::list_agents))
        .route("/v1/messages", post(messages::send))
        .route("/v1/messages/{id}", get(messages::get_message))
        .route("/v1/inbox", get(messages::list_inbox))
        .route("/v1/inbox/pull", post(messages::pull))
        .route("/v1/files", post(files::upload))
        .route("/v1/files/{id}", get(files::download))
        .route("/v1/groups", post(groups::create).get(groups::list))
        .route("/v1/groups/{id}/invites", post(groups::invite))
        .route("/v1/groups/{id}/sessions", put(groups::set_sessions))
        .route("/v1/groups/{id}/members/me", delete(groups::leave))
        .route(
            "/v1/groups/{id}/members/{account_id}",
            delete(groups::remove),
        )
        .route("/v1/groups/{id}/owner", put(groups::transfer))
        .route("/v1/groups/{id}", delete(groups::dissolve))
        .route("/v1/ws", get(ws::upgrade))
        .route("/v1/web/session", delete(auth::web_logout))
        .route("/v1/conversations", get(conversations::list))
        .route(
            "/v1/conversations/{id}/messages",
            get(conversations::messages),
        )
        .route(
            "/v1/agents/{id}/messages",
            get(conversations::agent_messages).post(messages::send_to_agent),
        )
        .route("/v1/agents/{id}/read", post(messages::mark_read))
        .route("/v1/agents/{id}/title", put(me::rename_agent))
        .route("/v1/billing", get(billing::get))
        .route("/v1/billing/checkout", post(billing::checkout))
        .route("/v1/billing/portal", post(billing::portal))
        .route(
            "/v1/billing/apple/transactions",
            post(billing::app_store_transaction),
        )
        .route("/v1/app/session", delete(app::logout))
        .route(
            "/v1/app/push",
            put(app::register_push).delete(app::unregister_push),
        )
        .route("/v1/account", delete(app::delete_account))
        .route(
            "/v1/me/avatar",
            put(avatars::set_mine).delete(avatars::delete_mine),
        )
        .route(
            "/v1/groups/{id}/avatar",
            put(avatars::set_group).delete(avatars::delete_group),
        )
        .route("/v1/blocks", post(safety::block).get(safety::list))
        .route("/v1/blocks/{account_id}", delete(safety::unblock))
        .route("/v1/reports", post(safety::report))
        .route_layer(middleware::from_fn_with_state(
            st.clone(),
            crate::auth::require,
        ));

    // ROLE-Admin only; anyone else gets 404 (DESIGN §10).
    let admin = Router::new()
        .route("/v1/admin/reports", get(admin::list_reports))
        .route("/v1/admin/reports/{id}", get(admin::get_report))
        .route(
            "/v1/admin/reports/{id}/resolve",
            post(admin::resolve_report),
        )
        .route("/v1/admin/accounts", get(admin::list_accounts))
        .route(
            "/v1/admin/accounts/{id}/ban",
            post(admin::ban).delete(admin::unban),
        )
        .route("/v1/admin/accounts/{id}", delete(admin::delete_account))
        .route("/v1/admin/stats", get(admin::stats))
        .route("/v1/admin/avatars/{id}", delete(admin::remove_avatar))
        .route_layer(middleware::from_fn_with_state(st.clone(), admin::gate));

    let mut public = Router::new()
        .route("/v1/auth/code", post(auth::request_code))
        .route("/v1/auth/verify", post(auth::verify))
        .route("/v1/health", get(health::health))
        .route("/v1/stats", get(health::stats))
        .route("/v1/web/session", post(auth::web_login))
        .route("/v1/app/session", post(auth::app_login))
        .route("/v1/billing/waffo/webhook", post(billing::webhook))
        .route(
            "/v1/billing/apple/notifications",
            post(billing::app_store_notification),
        )
        .route("/v1/avatars/{file}", get(avatars::get));
    if st.cfg.dev {
        public = public
            .route("/v1/dev/seed", post(dev::seed))
            .route("/v1/dev/expire-code", post(dev::expire_code))
            .route("/v1/dev/age-files", post(dev::age_files))
            .route("/v1/dev/age-account", post(dev::age_account));
    }

    Router::new()
        .merge(public)
        .merge(authed)
        .merge(admin)
        .fallback(|| async { err(ErrorCode::NotFound, "No such endpoint.") })
        .layer(middleware::from_fn(crate::auth::common))
        .layer(middleware::from_fn_with_state(
            st.clone(),
            crate::auth::cors,
        ))
        .with_state(st)
}

/// `Json` extractor whose rejection is a problem+json 400.
pub struct Body<T>(pub T);

impl<T, S> FromRequest<S> for Body<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, Self::Rejection> {
        match axum::Json::<T>::from_request(req, state).await {
            Ok(axum::Json(v)) => Ok(Body(v)),
            Err(e) => Err(rejection(e)),
        }
    }
}

fn rejection(e: JsonRejection) -> ApiError {
    err(
        ErrorCode::InvalidRequest,
        format!("Invalid JSON body: {}", e.body_text()),
    )
}
