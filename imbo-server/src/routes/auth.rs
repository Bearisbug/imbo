//! API-001 requestCode, API-002 verifyCode, API-028 webLogin, API-029 webLogout,
//! API-048 appLogin, API-075 requestEmailChange, API-076 confirmEmailChange.

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
    Account, AccountResponse, AppLoginRequest, AppLoginResponse, CodeRequest, CodeResponse,
    EmailChangeRequest, VerifyRequest, VerifyResponse, WebLoginRequest, WebLoginResponse,
};
use imbo_proto::text::{strip_control, valid_email, validate_name};
use rand::Rng;
use sha2::{Digest, Sha256};
use sqlx::{PgExecutor, Postgres, Transaction};
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
    limit_ip(&st, &headers, peer)?;
    wait_for_email(&st, &email).await?;

    // The store reviewers' address gets the fixed code, and no mail.
    let review = st.cfg.review_code(&email);
    let code = match review {
        Some(fixed) => fixed.to_string(),
        None => new_code(),
    };
    store_code(&st, &email, None, &code).await?;
    if review.is_none() {
        mail_code(&st, &email, &code).await?;
    }
    Ok((
        StatusCode::ACCEPTED,
        Json(CodeResponse { retry_after_s: 60 }),
    ))
}

/// At most 20 code requests an hour from one network, sign-in and email change together.
fn limit_ip(st: &St, headers: &HeaderMap, peer: SocketAddr) -> ApiResult<()> {
    let ip = client_ip(headers, peer);
    st.limiter
        .hit(&format!("ip:{ip}"), 20, Duration::from_secs(3600))
        .map_err(|wait| {
            err(
                ErrorCode::RateLimited,
                "Too many code requests from this network. Try again later.",
            )
            .retry_after(wait)
        })
}

/// One code a minute to an email, whoever asks.
async fn wait_for_email(st: &St, email: &str) -> ApiResult<()> {
    let last: Option<DateTime<Utc>> =
        sqlx::query_scalar("SELECT max(created_at) FROM email_codes WHERE lower(email) = $1")
            .bind(email)
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
    Ok(())
}

fn new_code() -> String {
    format!("{:06}", rand::rng().random_range(0..1_000_000u32))
}

/// Keep a new code for this email in place of the active ones of its kind: a
/// sign-in code has no account, an email-change code has the account that
/// asked, and neither kind replaces or passes for the other (ADR-047).
async fn store_code(st: &St, email: &str, account: Option<i32>, code: &str) -> ApiResult<()> {
    let salt: [u8; 16] = rand::random();
    let mut tx = st.db.begin().await?;
    sqlx::query(
        "UPDATE email_codes SET status = 'consumed'
         WHERE lower(email) = $1 AND account_id IS NOT DISTINCT FROM $2 AND status = 'active'",
    )
    .bind(email)
    .bind(account)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO email_codes (email, account_id, code_salt, code_hash, expires_at)
         VALUES ($1, $2, $3, $4, now() + make_interval(mins => $5))",
    )
    .bind(email)
    .bind(account)
    .bind(salt.as_slice())
    .bind(code_hash(&salt, code))
    .bind(CODE_TTL_MIN as i32)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

async fn mail_code(st: &St, email: &str, code: &str) -> ApiResult<()> {
    st.mailer.send_code(email, code).await.map_err(|e| {
        warn!(error = %e, "send code failed");
        err(
            ErrorCode::EmailSendFailed,
            "The code could not be emailed. Try again in a minute.",
        )
    })
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

/// The latest code of its kind for this email, if it matches: `account` is
/// `None` for a sign-in code, or the account an email-change code is for. A
/// wrong code counts toward the lock.
async fn check_code(st: &St, email: &str, account: Option<i32>, code: &str) -> ApiResult<CodeRow> {
    let row: Option<CodeRow> = sqlx::query_as(
        "SELECT id, code_salt, code_hash, status, expires_at FROM email_codes
         WHERE lower(email) = $1 AND account_id IS NOT DISTINCT FROM $2
           AND status IN ('active', 'locked')
         ORDER BY id DESC LIMIT 1",
    )
    .bind(email)
    .bind(account)
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
    Ok(row)
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
    let row = check_code(st, &email, None, code.trim()).await?;

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

/// `/errors/email-taken`, for API-075 and API-076.
fn email_taken() -> crate::error::ApiError {
    err(
        ErrorCode::EmailTaken,
        "That email is already used by another Imbo account.",
    )
}

/// Whether another account signs in with this email. A deleted account's
/// email was replaced, so it holds none.
async fn taken<'e>(db: impl PgExecutor<'e>, email: &str, me: i32) -> sqlx::Result<bool> {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM accounts WHERE lower(email) = $1 AND id <> $2)",
    )
    .bind(email)
    .bind(me)
    .fetch_one(db)
    .await
}

/// API-075: send a code to the email the caller wants to sign in with. Only
/// the new email gets one (ADR-047).
pub async fn request_email_change(
    State(st): State<St>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Extension(c): Extension<Caller>,
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
    let current: String = sqlx::query_scalar("SELECT lower(email) FROM accounts WHERE id = $1")
        .bind(c.account_id)
        .fetch_one(&st.db)
        .await?;
    if email == current {
        return Err(err(ErrorCode::InvalidEmail, "That's already your email."));
    }
    // The reviewers' address signs in with a fixed code; moving an account
    // onto it would let anyone sign in to that account.
    if st.cfg.review_code(&email).is_some() {
        return Err(err(
            ErrorCode::InvalidEmail,
            "That email can't be used. Choose another one.",
        ));
    }
    // A server that takes sign-ups only from some domains keeps its accounts
    // there too.
    if !st.cfg.signup_allowed(&email) {
        return Err(err(
            ErrorCode::SignupClosed,
            "This server doesn't accept emails from that domain.",
        ));
    }
    // Counted before the check below: a signed-in person could otherwise test
    // any number of emails for an Imbo account.
    limit_ip(&st, &headers, peer)?;
    if taken(&st.db, &email, c.account_id).await? {
        return Err(email_taken());
    }
    wait_for_email(&st, &email).await?;

    let code = new_code();
    store_code(&st, &email, Some(c.account_id), &code).await?;
    mail_code(&st, &email, &code).await?;
    info!(
        event = "email_change_code",
        account_id = c.account_id,
        "email change code sent"
    );
    Ok((
        StatusCode::ACCEPTED,
        Json(CodeResponse { retry_after_s: 60 }),
    ))
}

/// API-076: switch the caller's sign-in email once the code API-075 sent to it
/// matches. Every sign-in stays, and the old email hears nothing (ADR-047).
pub async fn change_email(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
    Body(req): Body<EmailChangeRequest>,
) -> ApiResult<Json<AccountResponse>> {
    let email = req.email.trim().to_lowercase();
    let row = check_code(&st, &email, Some(c.account_id), req.code.trim()).await?;

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
    // Someone signed up with it after the code went out: the code is spent all the same.
    if taken(&mut *tx, &email, c.account_id).await? {
        tx.commit().await?;
        return Err(email_taken());
    }
    // Two changes or a sign-up racing to the same email meet at the unique
    // index; the later one rolls back (DESIGN §16).
    let (name, avatar): (String, Option<Uuid>) = match sqlx::query_as(
        "UPDATE accounts SET email = $2 WHERE id = $1 RETURNING name, avatar_id",
    )
    .bind(c.account_id)
    .bind(&email)
    .fetch_one(&mut *tx)
    .await
    {
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => return Err(email_taken()),
        r => r?,
    };
    tx.commit().await?;
    let admin = c.web.is_some() && super::admin::is_admin(&st, c.account_id).await?;
    info!(
        event = "email_changed",
        account_id = c.account_id,
        "email changed"
    );
    Ok(Json(AccountResponse {
        account: Account {
            id: c.account_id,
            name,
            email: Some(email),
            avatar_url: crate::avatar::url_of(&st.cfg, avatar),
            admin,
        },
    }))
}
