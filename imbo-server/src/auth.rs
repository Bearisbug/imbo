//! Device tokens, the authenticated caller and the per-request middleware.

use std::time::Duration;

use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use base64::Engine;
use imbo_proto::{ErrorCode, HEADER_ACTOR, HEADER_REQUEST_ID, HEADER_UNREAD};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::error::{ApiResult, REQUEST_ID, err};
use crate::state::St;

pub fn new_token() -> String {
    let bytes: [u8; 32] = rand::random();
    format!(
        "imbo_{}",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    )
}

pub fn token_hash(token: &str) -> Vec<u8> {
    Sha256::digest(token.as_bytes()).to_vec()
}

/// The sending session resolved from `Imbo-Actor-Session`.
#[derive(Clone, Debug)]
pub struct Actor {
    pub id: Uuid,
    pub short_id: String,
    pub title: String,
}

#[derive(Clone, Debug)]
pub struct Caller {
    pub account_id: i32,
    pub name: String,
    pub device_id: Uuid,
    pub actor: Option<Actor>,
}

impl Caller {
    pub fn is_agent(&self) -> bool {
        self.actor.is_some()
    }

    /// Endpoints marked ✗ for ROLE-Agent in DESIGN §10.
    pub fn require_person(&self) -> ApiResult<()> {
        if self.actor.is_some() {
            return Err(err(
                ErrorCode::Forbidden,
                "Only the account owner can do this, from a terminal outside any agent session.",
            ));
        }
        Ok(())
    }

    pub fn decided_by(&self) -> &'static str {
        if self.actor.is_some() {
            "agent"
        } else {
            "person"
        }
    }
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
        .map(str::trim)
}

/// Resolve a device token to (account, name, device).
pub async fn device_for_token(st: &St, token: &str) -> ApiResult<(i32, String, Uuid)> {
    let row: Option<(i32, String, Uuid)> = sqlx::query_as(
        "SELECT a.id, a.name, d.id FROM devices d JOIN accounts a ON a.id = d.account_id
         WHERE d.token_hash = $1 AND d.revoked_at IS NULL",
    )
    .bind(token_hash(token))
    .fetch_optional(&st.db)
    .await?;
    row.ok_or_else(|| {
        err(
            ErrorCode::Unauthorized,
            "This computer is not signed in, or its sign-in was revoked. Run `imbo login`.",
        )
    })
}

async fn resolve(st: &St, headers: &HeaderMap) -> ApiResult<Caller> {
    let token = bearer(headers).ok_or_else(|| {
        err(
            ErrorCode::Unauthorized,
            "Missing device token. Run `imbo login`.",
        )
    })?;
    let (account_id, name, device_id) = device_for_token(st, token).await?;
    if let Err(wait) = st
        .limiter
        .hit(&format!("dev:{device_id}"), 600, Duration::from_secs(60))
    {
        return Err(err(
            ErrorCode::RateLimited,
            "Too many requests from this computer.",
        )
        .retry_after(wait));
    }
    sqlx::query(
        "UPDATE devices SET last_seen_at = now()
         WHERE id = $1 AND (last_seen_at IS NULL OR last_seen_at < now() - interval '60 seconds')",
    )
    .bind(device_id)
    .execute(&st.db)
    .await?;
    let actor = match headers.get(HEADER_ACTOR).and_then(|v| v.to_str().ok()) {
        None => None,
        Some(raw) => {
            let (tool, session) = raw.split_once(':').ok_or_else(|| {
                err(
                    ErrorCode::InvalidRequest,
                    "Imbo-Actor-Session must be <tool>:<session_id>.",
                )
            })?;
            let row: Option<(Uuid, String, String)> = sqlx::query_as(
                "SELECT id, short_id, title FROM agents WHERE account_id = $1 AND tool = $2 AND session_id = $3",
            )
            .bind(account_id)
            .bind(tool)
            .bind(session)
            .fetch_optional(&st.db)
            .await?;
            let (id, short_id, title) = row.ok_or_else(|| {
                err(
                    ErrorCode::UnknownSession,
                    "This session is not registered with your account yet.",
                )
            })?;
            Some(Actor {
                id,
                short_id,
                title,
            })
        }
    };
    Ok(Caller {
        account_id,
        name,
        device_id,
        actor,
    })
}

pub async fn unread_count(st: &St, agent: Uuid) -> Result<i64, sqlx::Error> {
    sqlx::query_scalar(
        "SELECT count(*) FROM deliveries WHERE agent_id = $1 AND state IN ('pending', 'pushed')",
    )
    .bind(agent)
    .fetch_one(&st.db)
    .await
}

/// Request id, no-store, and the problem `instance` for every route.
pub async fn common(req: Request, next: Next) -> Response {
    let id = req
        .headers()
        .get(HEADER_REQUEST_ID)
        .and_then(|v| v.to_str().ok())
        .filter(|v| !v.is_empty() && v.len() <= 64)
        .map(str::to_string)
        .unwrap_or_else(|| Uuid::now_v7().to_string());
    let mut res = REQUEST_ID.scope(id.clone(), next.run(req)).await;
    let h = res.headers_mut();
    h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    if let Ok(v) = HeaderValue::from_str(&id) {
        h.insert(HEADER_REQUEST_ID, v);
    }
    res
}

/// Authenticate, attach [`Caller`], and add `Imbo-Unread` for agent calls.
pub async fn require(State(st): State<St>, mut req: Request, next: Next) -> Response {
    let caller = match resolve(&st, req.headers()).await {
        Ok(c) => c,
        Err(e) => return e.into_response(),
    };
    let actor = caller.actor.as_ref().map(|a| a.id);
    req.extensions_mut().insert(caller);
    let mut res = next.run(req).await;
    if let Some(agent) = actor
        && let Ok(n) = unread_count(&st, agent).await
            && let Ok(v) = HeaderValue::from_str(&n.to_string()) {
                res.headers_mut().insert(HEADER_UNREAD, v);
            }
    res
}
