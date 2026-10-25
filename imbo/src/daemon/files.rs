//! Attachments through the daemon: upload (API-LOCAL-008) and download
//! into `~/.imbo/files/<message_id>/` (API-LOCAL-009, FLOW-DELIVER S3).

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use imbo_proto::api::{FileView, MessageResponse, MessageView, UploadResponse};
use imbo_proto::local::{FetchResponse, LocalFile, LocalResponse};
use imbo_proto::text::clean_file_name;
use imbo_proto::{ErrorCode, MAX_MESSAGE_FILE_BYTES, Problem};
use serde_json::json;
use tokio::time::Instant;
use tracing::{info, warn};
use uuid::Uuid;

use super::D;
use crate::files::{check_file, disk_free_bytes, local_paths, sha256_file};
use crate::net;
use crate::paths::Credentials;

/// An upload not yet sent with a message is reused for this long; the
/// server clears it after an hour (ENT-File ready → discarded).
const REUSE: Duration = Duration::from_secs(50 * 60);
/// Downloads leave at least this much free disk.
const DISK_MARGIN: u64 = 1024 * 1024 * 1024;
const TRIES: u32 = 3;

/// Recent uploads by (SHA-256, name).
pub type Recent = HashMap<(String, String), (FileView, Instant)>;

fn signed_in(d: &D) -> Result<Credentials, Problem> {
    d.creds().ok_or_else(|| {
        Problem::new(
            ErrorCode::NotLoggedIn,
            "This computer is not signed in to Imbo. Run `imbo login` in a terminal.",
        )
    })
}

fn revoked(d: &D) -> Problem {
    let mut l = d.link.lock().unwrap();
    l.revoked = true;
    l.last_error = Some("this computer's sign-in was revoked; run `imbo login`".into());
    Problem::new(
        ErrorCode::NotLoggedIn,
        "This computer's Imbo sign-in is no longer valid. Run `imbo login` in a terminal.",
    )
}

async fn hash(path: &Path) -> Result<(u64, String), Problem> {
    let p = path.to_path_buf();
    tokio::task::spawn_blocking(move || sha256_file(&p))
        .await
        .map_err(|e| Problem::new(ErrorCode::Unavailable, e.to_string()))?
        .map_err(|e| {
            Problem::new(
                ErrorCode::FileUnreadable,
                format!("{}: {e}", path.display()),
            )
        })
}

/// API-LOCAL-008.
pub async fn upload(d: &D, path: &str) -> LocalResponse {
    match try_upload(d, path).await {
        Ok(f) => LocalResponse::ok(201, json!(f), None),
        Err(p) => LocalResponse::err(p),
    }
}

async fn try_upload(d: &D, path: &str) -> Result<FileView, Problem> {
    let creds = signed_in(d)?;
    check_file(path)?;
    let file = Path::new(path);
    let name = clean_file_name(
        &file
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
    );
    let (mut size, mut sha) = hash(file).await?;
    let key = (sha.clone(), name.clone());
    if let Some((f, at)) = d.uploads.lock().unwrap().get(&key)
        && at.elapsed() < REUSE
    {
        info!(file = %f.id, "reusing an upload not sent yet");
        return Ok(f.clone());
    }
    for attempt in 0..2 {
        if size > MAX_MESSAGE_FILE_BYTES {
            return Err(Problem::new(
                ErrorCode::FileTooLarge,
                format!("{path} grew past 100 MB."),
            ));
        }
        match net::upload_file(&creds.server, &creds.token, file, &name, size, &sha).await {
            Ok(r) => {
                let up: UploadResponse = serde_json::from_value(r.body).map_err(|_| {
                    Problem::new(
                        ErrorCode::Unavailable,
                        "The server's reply to the upload was not understood; run `imbo setup` to update.",
                    )
                })?;
                info!(file = %up.file.id, size, used = up.quota.used, "uploaded");
                d.uploads
                    .lock()
                    .unwrap()
                    .insert((sha, name), (up.file.clone(), Instant::now()));
                return Ok(up.file);
            }
            // Changed while uploading: hash it again and resend once.
            Err(p) if p.is(ErrorCode::FileHashMismatch) && attempt == 0 => {
                warn!(path, "file changed during upload; retrying");
                (size, sha) = hash(file).await?;
            }
            Err(p) if p.is(ErrorCode::Unauthorized) => return Err(revoked(d)),
            // Only servers from before attachments answer 404 here.
            Err(p) if p.is(ErrorCode::NotFound) => {
                return Err(Problem::new(
                    ErrorCode::NotFound,
                    "This Imbo server doesn't support files yet. Ask its admin to upgrade it.",
                ));
            }
            Err(p) => return Err(p),
        }
    }
    Err(Problem::new(
        ErrorCode::FileHashMismatch,
        format!("{path} kept changing while it was uploaded."),
    ))
}

/// Uploads that went out with a message can't be reused.
pub fn forget(d: &D, sent: &[Uuid]) {
    d.uploads
        .lock()
        .unwrap()
        .retain(|_, (f, at)| !sent.contains(&f.id) && at.elapsed() < REUSE);
}

fn local(f: &FileView, path: Option<&Path>, error: Option<String>) -> LocalFile {
    LocalFile {
        id: f.id,
        name: f.name.clone(),
        size: f.size,
        path: path.map(|p| p.to_string_lossy().into_owned()),
        error,
    }
}

/// Download a message's attachments (or just `only`). One message at a
/// time per computer, so two sessions getting it share one download.
pub async fn fetch(d: &D, m: &MessageView, only: Option<Uuid>) -> Vec<LocalFile> {
    let lock = {
        let mut locks = d.fetch_locks.lock().unwrap();
        locks.retain(|_, l| Arc::strong_count(l) > 1);
        locks.entry(m.id).or_default().clone()
    };
    let _guard = lock.lock().await;
    let paths = local_paths(&d.home, m.id, &m.files);
    let mut out = vec![];
    for (f, path) in m.files.iter().zip(&paths) {
        if only.is_some_and(|id| id != f.id) {
            continue;
        }
        let error = fetch_one(d, f, path).await.err();
        if let Some(e) = &error {
            warn!(msg = m.id, file = %f.id, error = %e, "attachment not downloaded");
        }
        out.push(local(f, error.is_none().then_some(path.as_path()), error));
    }
    out
}

/// Like `fetch`, but returns after `budget`; the download goes on in the
/// background and files not done yet are marked `slow`.
pub async fn fetch_within(d: &D, m: &MessageView, budget: Duration) -> Vec<LocalFile> {
    let (d2, m2) = (d.clone(), m.clone());
    let mut task = tokio::spawn(async move { fetch(&d2, &m2, None).await });
    if let Ok(Ok(files)) = tokio::time::timeout(budget, &mut task).await {
        return files;
    }
    let paths = local_paths(&d.home, m.id, &m.files);
    m.files
        .iter()
        .zip(&paths)
        .map(|(f, p)| {
            let done = std::fs::metadata(p).is_ok_and(|x| x.len() == f.size);
            if done {
                local(f, Some(p), None)
            } else {
                local(f, None, Some("slow".into()))
            }
        })
        .collect()
}

/// `Err` is the `LocalFile.error` text.
async fn fetch_one(d: &D, f: &FileView, path: &Path) -> Result<(), String> {
    if std::fs::metadata(path).is_ok_and(|x| x.len() == f.size)
        && hash(path).await.is_ok_and(|(_, sha)| sha == f.sha256)
    {
        return Ok(());
    }
    let dir = path.parent().ok_or("failed: bad path")?;
    std::fs::create_dir_all(dir).map_err(|e| format!("failed: {e}"))?;
    if disk_free_bytes(dir).is_some_and(|free| free < f.size + DISK_MARGIN) {
        return Err("disk-full".into());
    }
    let creds = d.creds().ok_or("failed: not signed in")?;
    let part = path.with_file_name(format!(".{}.part", f.id));
    let mut last = String::new();
    for attempt in 1..=TRIES {
        let got = if crate::adapters::fault("download") {
            Err(Problem::new(ErrorCode::ServerUnreachable, "fault injected"))
        } else {
            net::download_file(&creds.server, &creds.token, f.id, &part).await
        };
        match got {
            Ok((size, sha)) if size == f.size && sha == f.sha256 => {
                std::fs::rename(&part, path).map_err(|e| format!("failed: {e}"))?;
                info!(file = %f.id, size, path = %path.display(), "attachment downloaded");
                return Ok(());
            }
            Ok(_) => last = "content didn't match".into(),
            Err(p) if p.is(ErrorCode::FileExpired) => {
                let _ = std::fs::remove_file(&part);
                return Err("expired".into());
            }
            Err(p) if p.is(ErrorCode::NotFound) => {
                let _ = std::fs::remove_file(&part);
                return Err("not-found".into());
            }
            Err(p) if p.is(ErrorCode::Unauthorized) => {
                let _ = std::fs::remove_file(&part);
                revoked(d);
                return Err("failed: this computer is not signed in".into());
            }
            Err(p) => last = p.detail,
        }
        let _ = std::fs::remove_file(&part);
        if attempt < TRIES {
            tokio::time::sleep(Duration::from_secs(attempt as u64 * 2)).await;
        }
    }
    Err(format!("failed: {last}"))
}

/// API-LOCAL-009 from the socket: look the message up, then download.
pub async fn fetch_request(d: &D, message_id: i64, only: Option<Uuid>) -> LocalResponse {
    let creds = match signed_in(d) {
        Ok(c) => c,
        Err(p) => return LocalResponse::err(p),
    };
    let r = net::request(
        &creds.server,
        "GET",
        &format!("/v1/messages/{message_id}"),
        Some(&creds.token),
        None,
        None,
    )
    .await;
    let m = match r.map(|r| serde_json::from_value::<MessageResponse>(r.body)) {
        Ok(Ok(r)) => r.message,
        Ok(Err(_)) => {
            return LocalResponse::err(Problem::new(
                ErrorCode::Unavailable,
                "The server's reply was not understood; run `imbo setup` to update.",
            ));
        }
        Err(p) if p.is(ErrorCode::Unauthorized) => return LocalResponse::err(revoked(d)),
        Err(p) => return LocalResponse::err(p),
    };
    let files = fetch(d, &m, only).await;
    LocalResponse::ok(200, json!(FetchResponse { files }), None)
}
