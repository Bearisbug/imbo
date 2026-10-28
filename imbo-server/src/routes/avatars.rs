//! API-045 setMyAvatar, API-046 deleteMyAvatar, API-047 getAvatar,
//! API-056 setGroupAvatar, API-057 deleteGroupAvatar (ADR-037).

use std::sync::{Arc, LazyLock};
use std::time::Duration;

use axum::Extension;
use axum::Json;
use axum::body::Body as RawBody;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use imbo_proto::ErrorCode;
use imbo_proto::api::AvatarResponse;
use tokio::sync::Semaphore;
use tracing::info;
use uuid::Uuid;

use super::groups::{not_owner, require_member, role_locked};
use crate::auth::Caller;
use crate::avatar;
use crate::error::{ApiResult, err};
use crate::state::St;

fn too_large() -> crate::error::ApiError {
    err(
        ErrorCode::FileTooLarge,
        "The picture can be at most 5 MB (5242880 bytes).",
    )
}

fn invalid() -> crate::error::ApiError {
    err(
        ErrorCode::ImageInvalid,
        "Only JPEG and PNG pictures can be avatars; convert it to JPEG or PNG first.",
    )
}

fn too_many_pixels() -> crate::error::ApiError {
    err(
        ErrorCode::ImageInvalid,
        "The picture has too many pixels; scale it down to about 4000x4000 or smaller first.",
    )
}

/// One picture is decoded at a time: even under the size check one can take
/// a few hundred MB while it is converted (DESIGN §15).
static DECODING: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(1)));

/// Read the picture in the request body and turn it into the stored JPEG.
async fn picture(headers: &HeaderMap, body: RawBody) -> ApiResult<Vec<u8>> {
    let declared = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    if declared.is_some_and(|n| n > avatar::MAX_BYTES as u64) {
        return Err(too_large());
    }
    let kind = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|v| {
            v.split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase()
        });
    if !matches!(kind.as_deref(), Some("image/jpeg" | "image/png")) {
        return Err(invalid());
    }
    let bytes = axum::body::to_bytes(body, avatar::MAX_BYTES)
        .await
        .map_err(|_| too_large())?;
    // The permit goes with the work, so it is held until decoding ends even
    // if the client goes away.
    let permit = DECODING
        .clone()
        .acquire_owned()
        .await
        .map_err(|e| anyhow::anyhow!("avatar queue: {e}"))?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        avatar::normalize(&bytes)
    })
    .await
    .map_err(|e| anyhow::anyhow!("avatar task: {e}"))?
    .map_err(|r| match r {
        avatar::Refused::NotImage => invalid(),
        avatar::Refused::TooManyPixels => too_many_pixels(),
    })
}

/// 20 changes a day per account, personal and group together (DESIGN §15).
fn count_change(st: &St, account: i32) -> ApiResult<()> {
    st.limiter
        .hit(
            &format!("avatar:{account}"),
            avatar::PER_DAY,
            Duration::from_secs(24 * 3600),
        )
        .map_err(|wait| {
            err(
                ErrorCode::RateLimited,
                "At most 20 avatar changes a day. Try again tomorrow.",
            )
            .retry_after(wait)
        })
}

/// API-045: only the web app and the App reach here (DESIGN §10).
pub async fn set_mine(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
    headers: HeaderMap,
    body: RawBody,
) -> ApiResult<Json<AvatarResponse>> {
    let jpeg = picture(&headers, body).await?;
    count_change(&st, c.account_id)?;
    let mut tx = st.db.begin().await?;
    // Two changes at once: the later commit stays (DESIGN §16).
    let old: Option<Uuid> =
        sqlx::query_scalar("SELECT avatar_id FROM accounts WHERE id = $1 FOR NO KEY UPDATE")
            .bind(c.account_id)
            .fetch_one(&mut *tx)
            .await?;
    // A deletion of this account that committed while we waited for the
    // row. Checked in its own statement: one started before the wait
    // wouldn't see it.
    let deleted: bool =
        sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM deleted_accounts WHERE id = $1)")
            .bind(c.account_id)
            .fetch_one(&mut *tx)
            .await?;
    if deleted {
        return Err(err(ErrorCode::Unauthorized, "This account was deleted."));
    }
    let id = avatar::insert(&mut tx, jpeg).await?;
    sqlx::query("UPDATE accounts SET avatar_id = $2 WHERE id = $1")
        .bind(c.account_id)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    avatar::delete(&mut tx, &old.into_iter().collect::<Vec<_>>()).await?;
    tx.commit().await?;
    info!(
        event = "avatar_set",
        account_id = c.account_id,
        "avatar changed"
    );
    Ok(Json(AvatarResponse {
        avatar_url: avatar::url(&st.cfg, id),
    }))
}

/// API-046: 204 whether or not there was one.
pub async fn delete_mine(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
) -> ApiResult<StatusCode> {
    let mut tx = st.db.begin().await?;
    let old: Option<Uuid> =
        sqlx::query_scalar("SELECT avatar_id FROM accounts WHERE id = $1 FOR NO KEY UPDATE")
            .bind(c.account_id)
            .fetch_one(&mut *tx)
            .await?;
    avatar::delete(&mut tx, &old.into_iter().collect::<Vec<_>>()).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}

/// API-047: public; an address's picture never changes, so it is cached
/// for a year.
pub async fn get(State(st): State<St>, Path(file): Path<String>) -> ApiResult<Response> {
    let missing = || err(ErrorCode::NotFound, "No such avatar.");
    let id = file
        .strip_suffix(".jpg")
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or_else(missing)?;
    let jpeg: Vec<u8> = sqlx::query_scalar("SELECT jpeg FROM avatars WHERE id = $1")
        .bind(id)
        .fetch_optional(&st.db)
        .await?
        .ok_or_else(missing)?;
    let mut res = jpeg.into_response();
    let h = res.headers_mut();
    h.insert(header::CONTENT_TYPE, HeaderValue::from_static("image/jpeg"));
    h.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=31536000, immutable"),
    );
    Ok(res)
}

/// API-056: the owner in person, from a terminal, the web app or the App.
pub async fn set_group(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
    Path(raw): Path<String>,
    headers: HeaderMap,
    body: RawBody,
) -> ApiResult<Json<AvatarResponse>> {
    c.require_person()?;
    let g = require_member(&st, &c, &raw).await?;
    let jpeg = picture(&headers, body).await?;
    let mut tx = st.db.begin().await?;
    if role_locked(&mut tx, &g, c.account_id).await? != "owner" {
        return Err(not_owner(&mut tx, &g, "change its picture").await?);
    }
    count_change(&st, c.account_id)?;
    let old: Option<Uuid> =
        sqlx::query_scalar("SELECT avatar_id FROM chat_groups WHERE id = $1 FOR NO KEY UPDATE")
            .bind(g.id)
            .fetch_one(&mut *tx)
            .await?;
    let id = avatar::insert(&mut tx, jpeg).await?;
    sqlx::query("UPDATE chat_groups SET avatar_id = $2 WHERE id = $1")
        .bind(g.id)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    avatar::delete(&mut tx, &old.into_iter().collect::<Vec<_>>()).await?;
    tx.commit().await?;
    info!(
        event = "avatar_set",
        account_id = c.account_id,
        group_id = g.id,
        "group avatar changed"
    );
    Ok(Json(AvatarResponse {
        avatar_url: avatar::url(&st.cfg, id),
    }))
}

/// API-057: the owner in person; 204 whether or not there was one.
pub async fn delete_group(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
    Path(raw): Path<String>,
) -> ApiResult<StatusCode> {
    c.require_person()?;
    let g = require_member(&st, &c, &raw).await?;
    let mut tx = st.db.begin().await?;
    if role_locked(&mut tx, &g, c.account_id).await? != "owner" {
        return Err(not_owner(&mut tx, &g, "change its picture").await?);
    }
    let old: Option<Uuid> =
        sqlx::query_scalar("SELECT avatar_id FROM chat_groups WHERE id = $1 FOR NO KEY UPDATE")
            .bind(g.id)
            .fetch_one(&mut *tx)
            .await?;
    avatar::delete(&mut tx, &old.into_iter().collect::<Vec<_>>()).await?;
    tx.commit().await?;
    Ok(StatusCode::NO_CONTENT)
}
