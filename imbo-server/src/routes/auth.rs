//! API-001 requestCode, API-002 verifyCode.

use std::net::SocketAddr;
use std::time::Duration;

use axum::Json;
use axum::extract::{ConnectInfo, State};
use axum::http::{HeaderMap, StatusCode};
use chrono::{DateTime, Utc};
use imbo_proto::ErrorCode;
use imbo_proto::api::{Account, CodeRequest, CodeResponse, VerifyRequest, VerifyResponse};
use imbo_proto::text::{strip_control, valid_email, validate_name};
use rand::Rng;
use sha2::{Digest, Sha256};
use tracing::{info, warn};
use uuid::Uuid;

use super::Body;
use crate::auth::{new_token, token_hash};
use crate::error::{ApiResult, err};
use crate::state::St;

const CODE_TTL_MIN: i64 = 10;
const MAX_ATTEMPTS: i32 = 5;

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

    let code = format!("{:06}", rand::rng().random_range(0..1_000_000u32));
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

    if let Err(e) = st.mailer.send_code(&email, &code).await {
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

#[derive(sqlx::FromRow)]
struct CodeRow {
    id: i64,
    code_salt: Vec<u8>,
    code_hash: Vec<u8>,
    status: String,
    expires_at: DateTime<Utc>,
}

pub async fn verify(
    State(st): State<St>,
    Body(req): Body<VerifyRequest>,
) -> ApiResult<Json<VerifyResponse>> {
    let email = req.email.trim().to_lowercase();
    let code = req.code.trim();
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

    let existing: Option<(i32, String, String)> =
        sqlx::query_as("SELECT id, name, email FROM accounts WHERE lower(email) = $1")
            .bind(&email)
            .fetch_optional(&st.db)
            .await?;
    let new_name = match (&existing, &req.name) {
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
    let (account, new_account) = match existing {
        Some((id, name, email)) => (
            Account {
                id,
                name,
                email: Some(email),
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
                },
                true,
            )
        }
    };
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
