//! Device tokens, the authenticated caller and the per-request middleware.

use std::time::Duration;

use axum::extract::{MatchedPath, Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
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
    /// Nil for the web app, which has no device (ADR-023).
    pub device_id: Uuid,
    pub actor: Option<Actor>,
    /// The web sign-in the request came with (ROLE-WebPerson).
    pub web: Option<Uuid>,
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

/// Name of the web sign-in cookie (API-028).
pub const WEB_COOKIE: &str = "imbo_session";

fn cookie<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == name)
        .map(|(_, v)| v.trim())
        .filter(|v| !v.is_empty())
}

async fn resolve_web(st: &St, token: &str) -> ApiResult<Caller> {
    let row: Option<(Uuid, i32, String)> = sqlx::query_as(
        "SELECT w.id, a.id, a.name FROM web_sessions w JOIN accounts a ON a.id = w.account_id
         WHERE w.token_hash = $1 AND w.revoked_at IS NULL AND w.expires_at > now()",
    )
    .bind(token_hash(token))
    .fetch_optional(&st.db)
    .await?;
    let (web, account_id, name) = row.ok_or_else(|| {
        err(
            ErrorCode::Unauthorized,
            "Your web sign-in has expired. Sign in again.",
        )
    })?;
    if let Err(wait) = st
        .limiter
        .hit(&format!("web:{web}"), 600, Duration::from_secs(60))
    {
        return Err(err(ErrorCode::RateLimited, "Too many requests.").retry_after(wait));
    }
    Ok(Caller {
        account_id,
        name,
        device_id: Uuid::nil(),
        actor: None,
        web: Some(web),
    })
}

async fn resolve(st: &St, headers: &HeaderMap) -> ApiResult<Caller> {
    let Some(token) = bearer(headers) else {
        if let Some(t) = cookie(headers, WEB_COOKIE) {
            return resolve_web(st, t).await;
        }
        return Err(err(
            ErrorCode::Unauthorized,
            "Missing device token. Run `imbo login`.",
        ));
    };
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
                "SELECT id, short_id, title FROM agents WHERE account_id = $1 AND tool = $2 AND session_id = $3 AND tool <> 'terminal'",
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
        web: None,
    })
}

/// What the web app may call: the ROLE-WebPerson column of DESIGN §10.
const WEB_ALLOWED: &[(Method, &str)] = &[
    (Method::GET, "/v1/me"),
    (Method::GET, "/v1/requests"),
    (Method::GET, "/v1/contacts"),
    (Method::GET, "/v1/agents"),
    (Method::GET, "/v1/agents/{id}/messages"),
    (Method::GET, "/v1/groups"),
    (Method::GET, "/v1/messages/{id}"),
    (Method::GET, "/v1/files/{id}"),
    (Method::GET, "/v1/conversations"),
    (Method::GET, "/v1/conversations/{id}/messages"),
    (Method::GET, "/v1/billing"),
    (Method::POST, "/v1/billing/checkout"),
    (Method::POST, "/v1/billing/portal"),
    (Method::DELETE, "/v1/web/session"),
];

/// What only the web app may call.
const WEB_ONLY: &[(Method, &str)] = &[
    (Method::POST, "/v1/billing/checkout"),
    (Method::POST, "/v1/billing/portal"),
    (Method::DELETE, "/v1/web/session"),
];

fn listed(list: &[(Method, &str)], method: &Method, path: &str) -> bool {
    list.iter().any(|(m, p)| m == method && *p == path)
}

/// Writes with a web sign-in must come from the web app's page (ADR-023).
pub fn require_web_origin(st: &St, headers: &HeaderMap) -> ApiResult<()> {
    let origin = headers.get(header::ORIGIN).and_then(|v| v.to_str().ok());
    match (&st.cfg.web_origin, origin) {
        (Some(want), Some(got)) if want == got => Ok(()),
        _ => Err(err(
            ErrorCode::Forbidden,
            "This request must come from the Imbo web app.",
        )),
    }
}

fn permitted(st: &St, c: &Caller, req: &Request) -> ApiResult<()> {
    let path = req
        .extensions()
        .get::<MatchedPath>()
        .map(MatchedPath::as_str)
        .unwrap_or_default();
    let method = req.method();
    if c.web.is_some() {
        if !listed(WEB_ALLOWED, method, path) {
            return Err(err(
                ErrorCode::Forbidden,
                "The web app can't do this. Use imbo on your computer.",
            ));
        }
        if method != Method::GET {
            require_web_origin(st, req.headers())?;
        }
    } else if listed(WEB_ONLY, method, path) {
        return Err(err(
            ErrorCode::Forbidden,
            "Only the Imbo web app can do this.",
        ));
    }
    Ok(())
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

/// Cross-origin access for the web app only, with cookies (DESIGN §8).
pub async fn cors(State(st): State<St>, req: Request, next: Next) -> Response {
    let origin = req
        .headers()
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .filter(|o| st.cfg.web_origin.as_deref() == Some(*o))
        .and_then(|o| HeaderValue::from_str(o).ok());
    let Some(origin) = origin else {
        return next.run(req).await;
    };
    let mut res = if req.method() == Method::OPTIONS {
        let mut r = StatusCode::NO_CONTENT.into_response();
        let h = r.headers_mut();
        h.insert(
            header::ACCESS_CONTROL_ALLOW_METHODS,
            HeaderValue::from_static("GET, POST, PATCH, PUT, DELETE"),
        );
        h.insert(
            header::ACCESS_CONTROL_ALLOW_HEADERS,
            HeaderValue::from_static("content-type, imbo-request-id"),
        );
        h.insert(
            header::ACCESS_CONTROL_MAX_AGE,
            HeaderValue::from_static("600"),
        );
        r
    } else {
        next.run(req).await
    };
    let h = res.headers_mut();
    h.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, origin);
    h.insert(
        header::ACCESS_CONTROL_ALLOW_CREDENTIALS,
        HeaderValue::from_static("true"),
    );
    h.insert(
        header::ACCESS_CONTROL_EXPOSE_HEADERS,
        HeaderValue::from_static("retry-after, imbo-request-id"),
    );
    h.append(header::VARY, HeaderValue::from_static("origin"));
    res
}

/// Authenticate, attach [`Caller`], and add `Imbo-Unread` for agent calls.
pub async fn require(State(st): State<St>, mut req: Request, next: Next) -> Response {
    let caller = match resolve(&st, req.headers()).await {
        Ok(c) => c,
        Err(e) => return e.into_response(),
    };
    if let Err(e) = permitted(&st, &caller, &req) {
        return e.into_response();
    }
    let actor = caller.actor.as_ref().map(|a| a.id);
    req.extensions_mut().insert(caller);
    let mut res = next.run(req).await;
    if let Some(agent) = actor
        && let Ok(n) = unread_count(&st, agent).await
        && let Ok(v) = HeaderValue::from_str(&n.to_string())
    {
        res.headers_mut().insert(HEADER_UNREAD, v);
    }
    res
}
