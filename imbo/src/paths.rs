//! Local files under `~/.imbo` (or `IMBO_HOME`).

use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const DEFAULT_SERVER: &str = "https://api.imbo.dev";

#[derive(Clone, Debug)]
pub struct Home {
    pub dir: PathBuf,
}

/// The login user's home from the passwd database, independent of `$HOME`.
fn passwd_home() -> Option<PathBuf> {
    unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if pw.is_null() || (*pw).pw_dir.is_null() {
            return None;
        }
        let s = std::ffi::CStr::from_ptr((*pw).pw_dir)
            .to_string_lossy()
            .into_owned();
        Some(PathBuf::from(s))
    }
}

pub fn user_home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(passwd_home)
        .unwrap_or_else(|| PathBuf::from("/tmp"))
}

impl Home {
    pub fn resolve() -> Self {
        let dir = match std::env::var_os("IMBO_HOME").filter(|v| !v.is_empty()) {
            Some(d) => PathBuf::from(d),
            None => user_home().join(".imbo"),
        };
        let dir = std::path::absolute(&dir).unwrap_or(dir);
        Home { dir }
    }

    /// True for the real user's `~/.imbo`; anything else (tests, temp homes)
    /// gets its own launchd/systemd label.
    pub fn is_primary(&self) -> bool {
        passwd_home().map(|h| h.join(".imbo")) == Some(self.dir.clone())
    }

    pub fn label(&self) -> String {
        if self.is_primary() {
            "dev.imbo.daemon".into()
        } else {
            let h = Sha256::digest(self.dir.to_string_lossy().as_bytes());
            format!("dev.imbo.daemon.{}", &hex::encode(h)[..8])
        }
    }

    pub fn socket(&self) -> PathBuf {
        self.dir.join("daemon.sock")
    }
    pub fn credentials_path(&self) -> PathBuf {
        self.dir.join("credentials")
    }
    pub fn config_path(&self) -> PathBuf {
        self.dir.join("config.toml")
    }
    pub fn logs(&self) -> PathBuf {
        self.dir.join("logs")
    }
    pub fn state(&self) -> PathBuf {
        self.dir.join("state")
    }
    pub fn plugin_dir(&self) -> PathBuf {
        self.dir.join("claude-plugin")
    }

    pub fn ensure(&self) -> anyhow::Result<()> {
        for d in [self.dir.clone(), self.logs(), self.state()] {
            std::fs::create_dir_all(&d).with_context(|| format!("create {}", d.display()))?;
        }
        std::fs::set_permissions(&self.dir, std::fs::Permissions::from_mode(0o700)).ok();
        Ok(())
    }

    pub fn config(&self) -> Config {
        std::fs::read_to_string(self.config_path())
            .ok()
            .and_then(|s| toml::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save_config(&self, cfg: &Config) -> anyhow::Result<()> {
        self.ensure()?;
        std::fs::write(self.config_path(), toml::to_string(cfg)?)?;
        Ok(())
    }

    pub fn credentials(&self) -> Option<Credentials> {
        let s = std::fs::read_to_string(self.credentials_path()).ok()?;
        toml::from_str(&s).ok()
    }

    pub fn save_credentials(&self, c: &Credentials) -> anyhow::Result<()> {
        self.ensure()?;
        write_private(&self.credentials_path(), toml::to_string(c)?.as_bytes())
    }

    pub fn clear_credentials(&self) {
        let _ = std::fs::remove_file(self.credentials_path());
    }
}

pub fn write_private(path: &Path, data: &[u8]) -> anyhow::Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)
            .with_context(|| format!("write {}", tmp.display()))?;
        f.write_all(data)?;
    }
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Config {
    pub server: String,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            server: DEFAULT_SERVER.into(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Credentials {
    pub server: String,
    pub token: String,
    pub device_id: String,
    pub account_id: i32,
    pub name: String,
    #[serde(default)]
    pub email: String,
}

/// `macos-aarch64`, `linux-x86_64`, …
pub fn platform() -> String {
    let os = std::env::consts::OS;
    format!("{os}-{}", std::env::consts::ARCH)
}

pub fn hostname() -> String {
    let mut buf = [0u8; 256];
    let ok = unsafe { libc::gethostname(buf.as_mut_ptr() as *mut libc::c_char, buf.len()) } == 0;
    if ok {
        let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
        let s = String::from_utf8_lossy(&buf[..end]).to_string();
        let s = s.trim_end_matches(".local").to_string();
        if !s.is_empty() {
            return s;
        }
    }
    "computer".into()
}

/// Directory holding the Claude Code state we read (`~/.claude`).
pub fn claude_dir() -> PathBuf {
    std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| user_home().join(".claude"))
}

pub fn codex_home() -> PathBuf {
    std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| user_home().join(".codex"))
}

/// Find an executable on PATH.
pub fn which(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(name))
        .find(|p| {
            p.metadata()
                .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
                .unwrap_or(false)
        })
}
