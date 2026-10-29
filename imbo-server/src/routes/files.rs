//! API-026 upload, API-027 download (ENT-File, ADR-011). Content lives in
//! `AppState::files_dir`; `file.cleanup` in `delivery` removes it.

use std::path::Path as FsPath;
use std::time::Duration;

use axum::Extension;
use axum::Json;
use axum::body::{Body as RawBody, Bytes};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, FixedOffset, TimeDelta, Utc};
use futures::StreamExt;
use imbo_proto::api::{FileView, Quota, UploadResponse};
use imbo_proto::text::{clean_file_name, percent_decode};
use imbo_proto::{
    DAILY_UPLOAD_BYTES, ErrorCode, HEADER_FILE_NAME, HEADER_FILE_SHA256, MAX_MESSAGE_FILE_BYTES,
};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::{info, warn};
use uuid::Uuid;

use super::health::disk_free_bytes;
use crate::auth::Caller;
use crate::error::{ApiError, ApiResult, err};
use crate::state::St;

/// A transfer with no bytes for this long is dropped (DESIGN §8 传文件).
const STALL: Duration = Duration::from_secs(30);
/// Uploads are refused below this much free space on the data disk.
const MIN_FREE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// Start of the current upload day and of the next one: midnight, Beijing time.
pub fn upload_day(now: DateTime<Utc>) -> (DateTime<Utc>, DateTime<Utc>) {
    let bj = FixedOffset::east_opt(8 * 3600).expect("valid offset");
    let midnight = now
        .with_timezone(&bj)
        .date_naive()
        .and_time(Default::default());
    let start = midnight
        .and_local_timezone(bj)
        .single()
        .expect("fixed offset")
        .with_timezone(&Utc);
    (start, start + TimeDelta::days(1))
}

fn mb(bytes: u64) -> String {
    format!("{:.1} MB", bytes as f64 / 1_048_576.0)
}

pub async fn upload(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
    headers: HeaderMap,
    body: RawBody,
) -> ApiResult<(StatusCode, Json<UploadResponse>)> {
    let declared: Option<u64> = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse().ok());
    if let Err(e) = crate::billing::require_active(&st, c.account_id, "API-026").await {
        return Err(refuse(body, declared, e).await);
    }
    let size =
        declared.ok_or_else(|| err(ErrorCode::InvalidRequest, "Content-Length is required."))?;
    if size > MAX_MESSAGE_FILE_BYTES {
        return Err(refuse(
            body,
            declared,
            err(ErrorCode::FileTooLarge, "A file can be at most 100 MB."),
        )
        .await);
    }
    let name = headers
        .get(HEADER_FILE_NAME)
        .and_then(|v| v.to_str().ok())
        .and_then(percent_decode)
        .filter(|n| !n.is_empty())
        .map(|n| clean_file_name(&n))
        .ok_or_else(|| {
            err(
                ErrorCode::InvalidRequest,
                "Imbo-File-Name must be the percent-encoded file name.",
            )
        })?;
    let sha: [u8; 32] = headers
        .get(HEADER_FILE_SHA256)
        .and_then(|v| v.to_str().ok())
        .filter(|v| v.len() == 64 && v.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')))
        .and_then(|v| hex::decode(v).ok())
        .and_then(|v| v.try_into().ok())
        .ok_or_else(|| {
            err(
                ErrorCode::InvalidRequest,
                "Imbo-File-Sha256 must be 64 lowercase hex digits.",
            )
        })?;
    let dir = st.files_dir();
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(anyhow::Error::from)?;
    if disk_free_bytes(&dir).is_some_and(|free| free < MIN_FREE_BYTES) {
        return Err(refuse(
            body,
            declared,
            err(
                ErrorCode::StorageFull,
                "The server is low on storage. Try again later.",
            ),
        )
        .await);
    }

    // Reserve the declared size under the account's lock so parallel
    // uploads can't overrun the day's limit together.
    let (day_start, resets_at) = upload_day(Utc::now());
    let id = Uuid::new_v4();
    let mut tx = st.db.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(1, $1)")
        .bind(c.account_id)
        .execute(&mut *tx)
        .await?;
    let used: i64 = sqlx::query_scalar(
        "SELECT coalesce(sum(size), 0)::bigint FROM files WHERE account_id = $1 AND created_at >= $2",
    )
    .bind(c.account_id)
    .bind(day_start)
    .fetch_one(&mut *tx)
    .await?;
    let used = used.max(0) as u64;
    if used + size > DAILY_UPLOAD_BYTES {
        // Not held while the body is read: the account's other uploads wait on it.
        tx.rollback().await?;
        let left = (resets_at - Utc::now()).num_minutes().max(1);
        let e = err(
            ErrorCode::UploadQuotaExceeded,
            format!(
                "You uploaded {} today and this file is {}; the limit is 1 GB a day. It resets at midnight Beijing time, in {} h {} min.",
                mb(used),
                mb(size),
                left / 60,
                left % 60
            ),
        );
        return Err(refuse(body, declared, e).await);
    }
    sqlx::query(
        "INSERT INTO files (id, account_id, name, size, sha256, expires_at)
         VALUES ($1, $2, $3, $4, $5, now() + interval '7 days')",
    )
    .bind(id)
    .bind(c.account_id)
    .bind(&name)
    .bind(size as i64)
    .bind(&sha[..])
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    let part = dir.join(format!("{id}.part"));
    let done = dir.join(id.to_string());
    let received = match receive(body, &part, size, &sha).await {
        Ok(()) => tokio::fs::rename(&part, &done)
            .await
            .map_err(|e| anyhow::Error::from(e).into()),
        Err(e) => Err(e),
    };
    if let Err(e) = received {
        // Interrupted or wrong content: drop it and give the quota back.
        let _ = tokio::fs::remove_file(&part).await;
        sqlx::query("DELETE FROM files WHERE id = $1 AND state = 'uploading'")
            .bind(id)
            .execute(&st.db)
            .await?;
        return Err(e);
    }
    let expires_at: Option<DateTime<Utc>> = sqlx::query_scalar(
        "UPDATE files SET state = 'ready', expires_at = now() + interval '7 days'
         WHERE id = $1 AND state = 'uploading' RETURNING expires_at",
    )
    .bind(id)
    .fetch_optional(&st.db)
    .await?;
    let Some(expires_at) = expires_at else {
        let _ = tokio::fs::remove_file(&done).await;
        return Err(err(
            ErrorCode::Unavailable,
            "The upload took too long and was cleared. Try again.",
        ));
    };
    info!(account_id = c.account_id, size, file = %id, "file_uploaded");
    Ok((
        StatusCode::CREATED,
        Json(UploadResponse {
            file: FileView {
                id,
                name,
                size,
                sha256: hex::encode(sha),
                expires_at,
            },
            quota: Quota {
                used: used + size,
                limit: DAILY_UPLOAD_BYTES,
                resets_at,
            },
        }),
    ))
}

/// Refuses an upload. A body declared at no more than 100 MB is read first:
/// clients that send the whole body before reading (iOS, browsers) lose an
/// answer that comes while they are still sending, as the connection closes
/// under them. A larger one is refused unread.
async fn refuse(body: RawBody, declared: Option<u64>, e: ApiError) -> ApiError {
    if declared.is_some_and(|n| n <= MAX_MESSAGE_FILE_BYTES) {
        let mut stream = body.into_data_stream();
        while let Ok(Some(Ok(_))) = tokio::time::timeout(STALL, stream.next()).await {}
    }
    e
}

/// Stream the request body into `part`, checking length and hash.
async fn receive(body: RawBody, part: &FsPath, size: u64, sha: &[u8; 32]) -> ApiResult<()> {
    let mut f = tokio::fs::File::create(part)
        .await
        .map_err(anyhow::Error::from)?;
    let mut stream = body.into_data_stream();
    let mut hasher = Sha256::new();
    let mut got: u64 = 0;
    loop {
        let next = tokio::time::timeout(STALL, stream.next())
            .await
            .map_err(|_| {
                err(
                    ErrorCode::InvalidRequest,
                    "No data for 30 seconds; the upload was dropped.",
                )
            })?;
        let Some(chunk) = next else { break };
        let chunk =
            chunk.map_err(|_| err(ErrorCode::InvalidRequest, "The upload was interrupted."))?;
        got += chunk.len() as u64;
        if got > size {
            return Err(err(
                ErrorCode::InvalidRequest,
                "The body is longer than Content-Length.",
            ));
        }
        hasher.update(&chunk);
        f.write_all(&chunk).await.map_err(anyhow::Error::from)?;
    }
    f.flush().await.map_err(anyhow::Error::from)?;
    if got != size {
        return Err(err(ErrorCode::InvalidRequest, "The upload ended early."));
    }
    if hasher.finalize()[..] != sha[..] {
        return Err(err(
            ErrorCode::FileHashMismatch,
            "The content doesn't match Imbo-File-Sha256; the file may have changed while uploading.",
        ));
    }
    Ok(())
}

pub async fn download(
    State(st): State<St>,
    Extension(c): Extension<Caller>,
    Path(id): Path<String>,
) -> ApiResult<Response> {
    let not_found = || err(ErrorCode::NotFound, "No such file.");
    let id = Uuid::parse_str(&id).map_err(|_| not_found())?;
    // The uploader, or anyone who can see the message (same rule as API-024).
    let row: Option<(String, i64, Vec<u8>, String)> = sqlx::query_as(
        "SELECT f.name, f.size, f.sha256, f.state FROM files f
         WHERE f.id = $1 AND (f.account_id = $2 OR EXISTS (
           SELECT 1 FROM messages m JOIN conversations cv ON cv.id = m.conversation_id
           WHERE m.id = f.message_id AND (
             m.from_account = $2 OR cv.account_lo = $2 OR cv.account_hi = $2
             OR EXISTS (SELECT 1 FROM group_members gm WHERE gm.group_id = cv.group_id AND gm.account_id = $2)
             OR EXISTS (SELECT 1 FROM deliveries d WHERE d.message_id = m.id AND d.account_id = $2))))",
    )
    .bind(id)
    .bind(c.account_id)
    .fetch_optional(&st.db)
    .await?;
    let Some((name, size, sha, state)) = row else {
        return Err(not_found());
    };
    match state.as_str() {
        "ready" | "sent" => {}
        "discarded" | "expired" => {
            return Err(err(
                ErrorCode::FileExpired,
                "This file has expired and is no longer on the server.",
            ));
        }
        _ => return Err(not_found()),
    }
    let file = match tokio::fs::File::open(st.files_dir().join(id.to_string())).await {
        Ok(f) => f,
        Err(e) => {
            warn!(file = %id, error = %e, "file content missing");
            return Err(not_found());
        }
    };
    let stream = futures::stream::unfold(Some(file), |f| async move {
        let mut f = f?;
        let mut buf = vec![0u8; 64 * 1024];
        match f.read(&mut buf).await {
            Ok(0) => None,
            Ok(n) => {
                buf.truncate(n);
                Some((Ok(Bytes::from(buf)), Some(f)))
            }
            Err(e) => Some((Err(e), None)),
        }
    });
    let mut res = RawBody::from_stream(stream).into_response();
    let h = res.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    h.insert(header::CONTENT_LENGTH, HeaderValue::from(size));
    if let Ok(v) = HeaderValue::from_str(&hex::encode(sha)) {
        h.insert(HEADER_FILE_SHA256, v);
    }
    if let Ok(v) = HeaderValue::from_str(&attachment(&name)) {
        h.insert(header::CONTENT_DISPOSITION, v);
    }
    Ok(res)
}

/// `Content-Disposition` that makes a browser save the file under its
/// original name (RFC 6266, with the RFC 8187 `filename*` form for non-ASCII).
fn attachment(name: &str) -> String {
    let mut encoded = String::new();
    for b in name.bytes() {
        if b.is_ascii_alphanumeric() || b"!#$&+-.^_`|~".contains(&b) {
            encoded.push(b as char);
        } else {
            encoded.push_str(&format!("%{b:02X}"));
        }
    }
    format!("attachment; filename*=UTF-8''{encoded}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_day_is_beijing_midnight() {
        // 2026-10-25 15:59 UTC is 23:59 in Beijing: still the 25th there.
        let t = DateTime::parse_from_rfc3339("2026-10-25T15:59:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let (start, end) = upload_day(t);
        assert_eq!(start.to_rfc3339(), "2026-10-24T16:00:00+00:00");
        assert_eq!(end.to_rfc3339(), "2026-10-25T16:00:00+00:00");
        // One minute later it's the 26th in Beijing.
        let (start, _) = upload_day(t + TimeDelta::minutes(1));
        assert_eq!(start.to_rfc3339(), "2026-10-25T16:00:00+00:00");
    }

    #[test]
    fn attachment_keeps_the_name() {
        assert_eq!(
            attachment("a b.txt"),
            "attachment; filename*=UTF-8''a%20b.txt"
        );
        assert_eq!(
            attachment("报告.pdf"),
            "attachment; filename*=UTF-8''%E6%8A%A5%E5%91%8A.pdf"
        );
    }
}
