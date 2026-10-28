//! API-001 requestCode, API-002 verifyCode, API-028 webLogin, API-029 webLogout,
//! API-048 appLogin.

use std::net::SocketAddr;
use std::time::Duration;

use axum::Extension;
use axum::Json;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use imbo_proto::ErrorCode;
use imbo_proto::api::{
    Account, AppLoginRequest, AppLoginResponse, CodeRequest, CodeResponse, VerifyRequest,
    VerifyResponse, WebLoginRequest, WebLoginResponse,
};
use imbo_proto::text::{strip_control, valid_email, validate_name};
use rand::Rng;
use sha2::{Digest, Sha256};
use sqlx::{Postgres, Transaction};
use tracing::{info, warn};
use uuid::Uuid;

use super::Body;
use crate::auth::{
    APP_DAYS, Caller, WEB_COOKIE, new_app_token, new_token, require_web_origin, token_hash,
};
use crate::error::{ApiResult, err};
use crate::state::St;

const CODE_TTL_MIN: i64 = 10;
const MAX_ATTEMPTS: i32 = 5;
/// A web sign-in lasts 30 days (ADR-023).
const WEB_DAYS: i64 = 30;

fn code_hash(salt: &[u8], code: &str) -> Vec<u8> {
    let mut h = Sha256::new();
    h.update(salt);
    h.update(code.as_bytes());
    h.finalize().to_vec()
}

fn client_ip(headers: &HeaderMap, peer: SocketAddr) -> String {
    // Only trust X-Forwarded-For when the peer is local (a reverse proxy on the box).
    if peer.ip().is_loopback()
        && let Some(v) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok())
        && let Some(first) = v.split(',').next()
    {
        return first.trim().to_string();
    }
    peer.ip().to_string()
}

pub async fn request_code(
    State(st): State<St>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Body(req): Body<CodeRequest>,
) -> ApiResult<(StatusCode, Json<CodeResponse>)> {
    let email = req.email.trim().to_lowercase();
    if !valid_email(&email) {
        return Err(err(
            ErrorCode::InvalidEmail,
            "That email address doesn't look right.",
        ));
    }
    if !st.cfg.signup_allowed(&email) {
        let exists: bool =
            sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM accounts WHERE lower(email) = $1)")
                .bind(&email)
                .fetch_one(&st.db)
                .await?;
        if !exists {
            return Err(err(
                ErrorCode::SignupClosed,
                "This server does not accept sign-ups from this email domain.",
            ));
        }
    }
    // RULE-014: a banned account gets no code.
    let banned: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM accounts WHERE lower(email) = $1 AND banned_at IS NOT NULL)",
    )
    .bind(&email)
    .fetch_one(&st.db)
    .await?;
    if banned {
        return Err(banned_error(&st));
    }
    let ip = client_ip(&headers, peer);
    if let Err(wait) = st
        .limiter
        .hit(&format!("ip:{ip}"), 20, Duration::from_secs(3600))
    {
        return Err(err(
            ErrorCode::RateLimited,
            "Too many code requests from this network. Try again later.",
        )
        .retry_after(wait));
    }
    let last: Option<DateTime<Utc>> =
        sqlx::query_scalar("SELECT max(created_at) FROM email_codes WHERE lower(email) = $1")
            .bind(&email)
            .fetch_one(&st.db)
            .await?;
    if let Some(last) = last {
        let age = (Utc::now() - last).num_seconds();
        if age < 60 {
            return Err(err(
                ErrorCode::RateLimited,
                "A code was just sent. Wait a minute before asking again.",
            )
            .retry_after((60 - age).max(1) as u64));
        }
    }

    // The store reviewers' address gets the fixed code, and no mail.
    let review = st.cfg.review_code(&email);
    let code = match review {
        Some(fixed) => fixed.to_string(),
        None => format!("{:06}", rand::rng().random_range(0..1_000_000u32)),
    };
    let salt: [u8; 16] = rand::random();
    let mut tx = st.db.begin().await?;
    // A new code replaces older active ones.
    sqlx::query(
        "UPDATE email_codes SET status = 'consumed' WHERE lower(email) = $1 AND status = 'active'",
    )
    .bind(&email)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO email_codes (email, code_salt, code_hash, expires_at)
         VALUES ($1, $2, $3, now() + make_interval(mins => $4))",
    )
    .bind(&email)
    .bind(salt.as_slice())
    .bind(code_hash(&salt, &code))
    .bind(CODE_TTL_MIN as i32)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    if review.is_none()
        && let Err(e) = st.mailer.send_code(&email, &code).await
    {
        warn!(error = %e, "send code failed");
        return Err(err(
            ErrorCode::EmailSendFailed,
            "The code could not be emailed. Try again in a minute.",
        ));
    }
    Ok((
        StatusCode::ACCEPTED,
        Json(CodeResponse { retry_after_s: 60 }),
    ))
}

/// `/errors/account-banned`, with whom to write to (DESIGN §14).
fn banned_error(st: &St) -> crate::error::ApiError {
    let contact = match &st.cfg.report_email {
        Some(e) => format!("write to {e}"),
        None => "contact the server's operator".to_string(),
    };
    err(
        ErrorCode::AccountBanned,
        format!("This account has been banned. If you think this is a mistake, {contact}."),
    )
}

#[derive(sqlx::FromRow)]
struct CodeRow {
    id: i64,
    code_salt: Vec<u8>,
    code_hash: Vec<u8>,
    status: String,
    expires_at: DateTime<Utc>,
}

/// Check the code and find or create the account. The code is consumed and
/// the account written in the returned transaction; the caller adds its
/// sign-in and commits.
async fn sign_in(
    st: &St,
    email: &str,
    code: &str,
    name: Option<&String>,
) -> ApiResult<(Transaction<'static, Postgres>, Account, bool)> {
    let email = email.trim().to_lowercase();
    let code = code.trim();
    let row: Option<CodeRow> = sqlx::query_as(
        "SELECT id, code_salt, code_hash, status, expires_at FROM email_codes
         WHERE lower(email) = $1 AND status IN ('active', 'locked') ORDER BY id DESC LIMIT 1",
    )
    .bind(&email)
    .fetch_optional(&st.db)
    .await?;
    let Some(row) = row else {
        return Err(err(
            ErrorCode::CodeInvalid,
            "That code is not valid. Request a new one.",
        ));
    };
    if row.status == "locked" {
        return Err(err(
            ErrorCode::CodeLocked,
            "Too many wrong codes. Request a new code.",
        ));
    }
    if Utc::now() > row.expires_at {
        return Err(err(
            ErrorCode::CodeExpired,
            "That code has expired. Request a new one.",
        ));
    }
    if code_hash(&row.code_salt, code) != row.code_hash {
        let attempts: i32 = sqlx::query_scalar(
            "UPDATE email_codes SET attempts = attempts + 1,
                    status = CASE WHEN attempts + 1 >= $2 THEN 'locked' ELSE status END
             WHERE id = $1 AND status = 'active' RETURNING attempts",
        )
        .bind(row.id)
        .bind(MAX_ATTEMPTS)
        .fetch_optional(&st.db)
        .await?
        .unwrap_or(MAX_ATTEMPTS);
        if attempts >= MAX_ATTEMPTS {
            return Err(err(
                ErrorCode::CodeLocked,
                "Too many wrong codes. Request a new code.",
            ));
        }
        let left = (MAX_ATTEMPTS - attempts) as u32;
        return Err(err(
            ErrorCode::CodeInvalid,
            format!("Wrong code. {left} tries left."),
        )
        .attempts_left(left));
    }

    let existing: Option<(i32, String, String, Option<Uuid>)> =
        sqlx::query_as("SELECT id, name, email, avatar_id FROM accounts WHERE lower(email) = $1")
            .bind(&email)
            .fetch_optional(&st.db)
            .await?;
    let new_name = match (&existing, name) {
        (Some(_), _) => None,
        (None, None) => {
            return Err(err(
                ErrorCode::NameRequired,
                "New account: choose a name first.",
            ));
        }
        (None, Some(n)) => Some(validate_name(n).map_err(|m| err(ErrorCode::InvalidName, m))?),
    };
    if existing.is_none() && !st.cfg.signup_allowed(&email) {
        return Err(err(
            ErrorCode::SignupClosed,
            "This server does not accept sign-ups from this email domain.",
        ));
    }

    let mut tx = st.db.begin().await?;
    let consumed = sqlx::query(
        "UPDATE email_codes SET status = 'consumed' WHERE id = $1 AND status = 'active'",
    )
    .bind(row.id)
    .execute(&mut *tx)
    .await?;
    if consumed.rows_affected() == 0 {
        return Err(err(
            ErrorCode::CodeInvalid,
            "That code was already used. Request a new one.",
        ));
    }
    if let Some((id, ..)) = &existing {
        // A ban in progress finishes first; this sign-in then sees it, or
        // commits first and is ended by the ban (DESIGN §16).
        let banned: bool = sqlx::query_scalar(
            "SELECT banned_at IS NOT NULL FROM accounts WHERE id = $1 FOR SHARE",
        )
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
        if banned {
            return Err(banned_error(st));
        }
    }
    let (account, new_account) = match existing {
        Some((id, name, email, avatar)) => (
            Account {
                id,
                name,
                email: Some(email),
                avatar_url: crate::avatar::url_of(&st.cfg, avatar),
                admin: false,
            },
            false,
        ),
        None => {
            let name = new_name.unwrap_or_default();
            let mut id = 0;
            for _ in 0..20 {
                let candidate = rand::rng().random_range(10_000_000..=99_999_999);
                let inserted = sqlx::query(
                    "INSERT INTO accounts (id, name, email) VALUES ($1, $2, $3) ON CONFLICT (id) DO NOTHING",
                )
                .bind(candidate)
                .bind(&name)
                .bind(&email)
                .execute(&mut *tx)
                .await?;
                if inserted.rows_affected() == 1 {
                    id = candidate;
                    break;
                }
            }
            if id == 0 {
                return Err(err(
                    ErrorCode::Unavailable,
                    "Could not allocate an account ID. Try again.",
                ));
            }
            (
                Account {
                    id,
                    name,
                    email: Some(email.clone()),
                    avatar_url: None,
                    admin: false,
                },
                true,
            )
        }
    };
    Ok((tx, account, new_account))
}

pub async fn verify(
    State(st): State<St>,
    Body(req): Body<VerifyRequest>,
) -> ApiResult<Json<VerifyResponse>> {
    let (mut tx, account, new_account) =
        sign_in(&st, &req.email, &req.code, req.name.as_ref()).await?;
    let token = new_token();
    let device_id = Uuid::now_v7();
    let device_name: String = strip_control(req.device_name.trim(), false)
        .chars()
        .take(64)
        .collect();
    let platform: String = strip_control(req.platform.trim(), false)
        .chars()
        .take(64)
        .collect();
    let version: String = strip_control(req.client_version.trim(), false)
        .chars()
        .take(32)
        .collect();
    sqlx::query(
        "INSERT INTO devices (id, account_id, name, platform, client_version, token_hash, last_seen_at)
         VALUES ($1, $2, $3, $4, $5, $6, now())",
    )
    .bind(device_id)
    .bind(account.id)
    .bind(if device_name.is_empty() { "computer".to_string() } else { device_name })
    .bind(platform)
    .bind(version)
    .bind(token_hash(&token))
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    info!(account = account.id, new_account, "signed in");
    Ok(Json(VerifyResponse {
        account,
        device_id,
        token,
        new_account,
    }))
}

fn session_cookie(value: &str, max_age: i64) -> HeaderValue {
    HeaderValue::from_str(&format!(
        "{WEB_COOKIE}={value}; Path=/; Max-Age={max_age}; HttpOnly; Secure; SameSite=Lax"
    ))
    .expect("cookie is ASCII")
}

/// API-028: sign the web app in with an email code; the token only lives in
/// an HttpOnly cookie (ADR-023).
pub async fn web_login(
    State(st): State<St>,
    headers: HeaderMap,
    Body(req): Body<WebLoginRequest>,
) -> ApiResult<Response> {
    require_web_origin(&st, &headers)?;
    let (mut tx, account, new_account) =
        sign_in(&st, &req.email, &req.code, req.name.as_ref()).await?;
    let token = new_token();
    sqlx::query(
        "INSERT INTO web_sessions (id, account_id, token_hash, expires_at)
         VALUES ($1, $2, $3, now() + make_interval(days => $4))",
    )
    .bind(Uuid::now_v7())
    .bind(account.id)
    .bind(token_hash(&token))
    .bind(WEB_DAYS as i32)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    info!(
        event = "web_login",
        account_id = account.id,
        new_account,
        "web signed in"
    );
    let mut res = Json(WebLoginResponse {
        account,
        new_account,
    })
    .into_response();
    res.headers_mut().insert(
        header::SET_COOKIE,
        session_cookie(&token, WEB_DAYS * 24 * 3600),
    );
    Ok(res)
}

/// API-029: end this web sign-in and clear the cookie.
pub async fn web_logout(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
) -> ApiResult<Response> {
    sqlx::query("UPDATE web_sessions SET revoked_at = now() WHERE id = $1 AND revoked_at IS NULL")
        .bind(c.web)
        .execute(&st.db)
        .await?;
    let mut res = StatusCode::NO_CONTENT.into_response();
    res.headers_mut()
        .insert(header::SET_COOKIE, session_cookie("", 0));
    Ok(res)
}

/// API-048: sign the App in with an email code; the token goes in the
/// `Authorization` header and needs no `Origin` (ADR-033).
pub async fn app_login(
    State(st): State<St>,
    Body(req): Body<AppLoginRequest>,
) -> ApiResult<Json<AppLoginResponse>> {
    let platform = req.platform.trim();
    if !matches!(platform, "ios" | "android") {
        return Err(err(
            ErrorCode::InvalidRequest,
            "platform must be ios or android.",
        ));
    }
    let (mut tx, account, new_account) =
        sign_in(&st, &req.email, &req.code, req.name.as_ref()).await?;
    let token = new_app_token();
    sqlx::query(
        "INSERT INTO app_sessions (id, account_id, token_hash, platform, expires_at)
         VALUES ($1, $2, $3, $4, now() + make_interval(days => $5))",
    )
    .bind(Uuid::now_v7())
    .bind(account.id)
    .bind(token_hash(&token))
    .bind(platform)
    .bind(APP_DAYS)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    info!(
        event = "app_login",
        account_id = account.id,
        new_account,
        platform,
        "app signed in"
    );
    Ok(Json(AppLoginResponse {
        account,
        token,
        new_account,
    }))
}
