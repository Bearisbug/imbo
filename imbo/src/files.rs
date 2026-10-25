//! Local side of attachments: checking files before upload, hashing, and
//! where received files go (`~/.imbo/files/<message_id>/`).

use std::io::Read;
use std::path::{Path, PathBuf};

use imbo_proto::api::FileView;
use imbo_proto::text::clean_file_name;
use imbo_proto::{ErrorCode, MAX_FILES, MAX_MESSAGE_FILE_BYTES, Problem};
use sha2::{Digest, Sha256};

use crate::paths::Home;

/// Size and SHA-256 (lowercase hex) of a file.
pub fn sha256_file(path: &Path) -> std::io::Result<(u64, String)> {
    let mut f = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    let mut size = 0u64;
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        size += n as u64;
        hasher.update(&buf[..n]);
    }
    Ok((size, hex::encode(hasher.finalize())))
}

pub fn human_size(bytes: u64) -> String {
    const KB: f64 = 1024.0;
    let b = bytes as f64;
    if b < KB {
        format!("{bytes} B")
    } else if b < KB * KB {
        format!("{:.1} KB", b / KB)
    } else if b < KB * KB * KB {
        format!("{:.1} MB", b / KB / KB)
    } else {
        format!("{:.1} GB", b / KB / KB / KB)
    }
}

/// Free bytes on the disk holding `path`.
#[allow(clippy::unnecessary_cast)] // field widths differ between macOS and Linux
pub fn disk_free_bytes(path: &Path) -> Option<u64> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let c = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut s: libc::statvfs = unsafe { std::mem::zeroed() };
    if unsafe { libc::statvfs(c.as_ptr(), &mut s) } != 0 {
        return None;
    }
    Some(s.f_bavail as u64 * s.f_frsize as u64)
}

pub fn message_dir(home: &Home, message_id: i64) -> PathBuf {
    home.dir.join("files").join(message_id.to_string())
}

/// File names on disk for a message's attachments: cleaned again (the
/// sender's server may be older or hostile), and a repeated name gets
/// " (2)", " (3)" before its extension.
pub fn local_names(files: &[FileView]) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for f in files {
        let name = clean_file_name(&f.name);
        let mut candidate = name.clone();
        let mut n = 2;
        while out.iter().any(|o| o.eq_ignore_ascii_case(&candidate)) {
            candidate = match name.rsplit_once('.').filter(|(stem, _)| !stem.is_empty()) {
                Some((stem, ext)) => format!("{stem} ({n}).{ext}"),
                None => format!("{name} ({n})"),
            };
            n += 1;
        }
        out.push(candidate);
    }
    out
}

/// Where each attachment of message `message_id` goes on this computer.
pub fn local_paths(home: &Home, message_id: i64, files: &[FileView]) -> Vec<PathBuf> {
    let dir = message_dir(home, message_id);
    local_names(files)
        .into_iter()
        .map(|n| dir.join(n))
        .collect()
}

fn unreadable(path: &str, why: &str) -> Problem {
    Problem::new(ErrorCode::FileUnreadable, format!("{path}: {why}"))
}

/// One file to upload: an absolute path to a readable regular file of at
/// most 100 MB. Returns its size.
pub fn check_file(path: &str) -> Result<u64, Problem> {
    if !Path::new(path).is_absolute() {
        return Err(Problem::new(
            ErrorCode::InvalidRequest,
            format!("{path}: write the file's absolute path."),
        ));
    }
    let meta = std::fs::metadata(path).map_err(|e| unreadable(path, &e.to_string()))?;
    if !meta.is_file() {
        return Err(unreadable(path, "not a regular file"));
    }
    std::fs::File::open(path).map_err(|e| unreadable(path, &e.to_string()))?;
    if meta.len() > MAX_MESSAGE_FILE_BYTES {
        return Err(Problem::new(
            ErrorCode::FileTooLarge,
            format!(
                "{path} is {}; a file can be at most 100 MB.",
                human_size(meta.len())
            ),
        ));
    }
    Ok(meta.len())
}

/// The files of one message, checked before anything is uploaded
/// (FLOW-FILE S2): at most 20, each readable, 100 MB together.
pub fn check_files(paths: &[String]) -> Result<(), Problem> {
    if paths.len() > MAX_FILES {
        return Err(Problem::new(
            ErrorCode::InvalidRequest,
            format!(
                "A message can carry at most {MAX_FILES} files (got {}).",
                paths.len()
            ),
        ));
    }
    let mut total = 0;
    for p in paths {
        total += check_file(p)?;
    }
    if total > MAX_MESSAGE_FILE_BYTES {
        return Err(Problem::new(
            ErrorCode::FileTooLarge,
            format!(
                "These files are {} together; one message can carry at most 100 MB.",
                human_size(total)
            ),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fv(name: &str) -> FileView {
        FileView {
            id: uuid::Uuid::nil(),
            name: name.into(),
            size: 1,
            sha256: String::new(),
            expires_at: chrono::Utc::now(),
        }
    }

    #[test]
    fn repeated_names_get_numbers() {
        let names = local_names(&[
            fv("a.txt"),
            fv("A.txt"),
            fv("a.txt"),
            fv("../x"),
            fv(".env"),
        ]);
        assert_eq!(names, ["a.txt", "A (2).txt", "a (3).txt", ".._x", ".env"]);
    }

    #[test]
    fn sizes() {
        assert_eq!(human_size(512), "512 B");
        assert_eq!(human_size(1536), "1.5 KB");
        assert_eq!(human_size(104_857_600), "100.0 MB");
    }

    #[test]
    fn relative_and_missing_paths() {
        assert!(
            check_file("notes.txt")
                .unwrap_err()
                .is(ErrorCode::InvalidRequest)
        );
        assert!(
            check_file("/nonexistent/imbo-test")
                .unwrap_err()
                .is(ErrorCode::FileUnreadable)
        );
        assert!(check_file("/").unwrap_err().is(ErrorCode::FileUnreadable));
    }
}
