//! New versions: the daemon's daily check, the hint after terminal commands,
//! and `imbo update` (REQ-046, REQ-047, ADR-026).

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context, bail};
use sha2::{Digest, Sha256};

use crate::paths::{Home, exe};
use crate::ui;

const DEFAULT_BASE: &str = "https://github.com/Bearisbug/imbo/releases/latest/download";

/// Where releases are downloaded from; `IMBO_DOWNLOAD_BASE` as in install.sh.
pub fn base() -> String {
    std::env::var("IMBO_DOWNLOAD_BASE")
        .ok()
        .filter(|b| !b.is_empty())
        .unwrap_or_else(|| DEFAULT_BASE.into())
        .trim_end_matches('/')
        .to_string()
}

fn parse(v: &str) -> Option<([u64; 3], bool)> {
    let (core, pre) = match v.trim().split_once('-') {
        Some((c, _)) => (c, true),
        None => (v.trim(), false),
    };
    let mut n = core.split('.').map(|p| p.parse::<u64>().ok());
    let r = [n.next()??, n.next()??, n.next()??];
    n.next().is_none().then_some((r, pre))
}

/// True when version `a` is newer than `b`; false when either doesn't parse.
pub fn newer(a: &str, b: &str) -> bool {
    match (parse(a), parse(b)) {
        // A release sorts after its own pre-releases.
        (Some((x, xp)), Some((y, yp))) => x > y || (x == y && !xp && yp),
        _ => false,
    }
}

fn curl(args: &[&str]) -> anyhow::Result<Vec<u8>> {
    let out = Command::new("curl")
        .args(["-fsSL", "--max-time", "30"])
        .args(args)
        .stdin(Stdio::null())
        .output()
        .context("run curl")?;
    if !out.status.success() {
        bail!(
            "{}",
            String::from_utf8_lossy(&out.stderr)
                .trim()
                .trim_start_matches("curl: ")
        );
    }
    Ok(out.stdout)
}

/// The latest released version, read from the release's `version` file.
pub fn fetch_latest() -> anyhow::Result<String> {
    let url = format!("{}/version", base());
    let body = curl(&[&url]).with_context(|| format!("download {url}"))?;
    let v = String::from_utf8_lossy(&body).trim().to_string();
    if parse(&v).is_none() {
        bail!("{url} doesn't hold a version number");
    }
    Ok(v)
}

fn latest_path(home: &Home) -> PathBuf {
    home.state().join("latest-version")
}

pub fn save_latest(home: &Home, v: &str) {
    let _ = home.ensure();
    let _ = std::fs::write(latest_path(home), format!("{v}\n"));
}

/// The newest version the daemon found, when it is newer than this program.
pub fn available(home: &Home) -> Option<String> {
    let v = std::fs::read_to_string(latest_path(home)).ok()?;
    let v = v.trim();
    newer(v, crate::VERSION).then(|| v.to_string())
}

/// One line after a command run in a terminal when a newer version is out.
pub fn hint() {
    if !console::Term::stderr().is_term() {
        return;
    }
    if let Some(v) = available(&Home::resolve()) {
        eprintln!(
            "{}",
            ui::dim(format!(
                "imbo {v} is out (you have {}) · run `imbo update`",
                crate::VERSION
            ))
        );
    }
}

fn platform() -> anyhow::Result<String> {
    let os = match std::env::consts::OS {
        "macos" => "macos",
        "linux" => "linux",
        o => bail!("unsupported system {o}"),
    };
    let arch = match std::env::consts::ARCH {
        "aarch64" => "aarch64",
        "x86_64" => "x86_64",
        a => bail!("unsupported chip {a}"),
    };
    Ok(format!("{os}-{arch}"))
}

/// Download, check and unpack the package; returns the new program's path.
fn download(base: &str, tmp: &Path) -> anyhow::Result<PathBuf> {
    let pkg = format!("imbo-{}.tar.gz", platform()?);
    let pkg_path = tmp.join(&pkg);
    curl(&["-o", &pkg_path.to_string_lossy(), &format!("{base}/{pkg}")])
        .with_context(|| format!("download {base}/{pkg}"))?;
    let sums = curl(&[&format!("{base}/checksums.txt")])
        .with_context(|| format!("download {base}/checksums.txt"))?;
    let sums = String::from_utf8_lossy(&sums);
    let want = sums
        .lines()
        .find_map(|l| l.strip_suffix(&format!(" {pkg}")))
        .map(|h| h.trim().to_string())
        .with_context(|| format!("checksum mismatch: {pkg} is not listed in checksums.txt"))?;
    let got = hex::encode(Sha256::digest(std::fs::read(&pkg_path)?));
    if want != got {
        bail!("checksum mismatch for {pkg} (expected {want}, got {got})");
    }
    let st = Command::new("tar")
        .arg("-xzf")
        .arg(&pkg_path)
        .arg("-C")
        .arg(tmp)
        .status()
        .context("run tar")?;
    let bin = tmp.join("imbo");
    if !st.success() || !bin.is_file() {
        bail!("the package has no imbo program");
    }
    Ok(bin)
}

/// The version a program reports, e.g. "0.2.11" from "imbo 0.2.11".
fn version_of(bin: &Path) -> anyhow::Result<String> {
    let out = Command::new(bin)
        .arg("--version")
        .output()
        .context("run the new program")?;
    String::from_utf8_lossy(&out.stdout)
        .trim()
        .strip_prefix("imbo ")
        .filter(|v| out.status.success() && parse(v).is_some())
        .map(str::to_string)
        .context("the new program doesn't run on this computer")
}

/// Put `new` in place of `target`: copied next to it, then renamed over it.
fn replace(new: &Path, target: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let staged = target.with_file_name(format!(".imbo-update-{}", std::process::id()));
    std::fs::copy(new, &staged).with_context(|| format!("write {}", staged.display()))?;
    std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))?;
    std::fs::rename(&staged, target).inspect_err(|_| {
        let _ = std::fs::remove_file(&staged);
    })?;
    Ok(())
}

/// `imbo update`.
pub async fn update() -> anyhow::Result<()> {
    let base = base();
    let latest = fetch_latest().map_err(|e| failed(&e))?;
    if !newer(&latest, crate::VERSION) {
        save_latest(&Home::resolve(), &latest);
        ui::ok_line(&format!("imbo {} is the latest version", crate::VERSION));
        return Ok(());
    }
    let target = exe().canonicalize().unwrap_or_else(|_| exe());
    let tmp = std::env::temp_dir().join(format!("imbo-update.{}", std::process::id()));
    std::fs::create_dir_all(&tmp)?;
    eprintln!(
        "{}",
        ui::dim(format!("downloading imbo {latest} from {base}"))
    );
    let res = download(&base, &tmp).and_then(|bin| {
        let v = version_of(&bin)?;
        replace(&bin, &target)?;
        Ok(v)
    });
    let _ = std::fs::remove_dir_all(&tmp);
    let got = res.map_err(|e| failed(&e))?;
    ui::ok_line(&format!(
        "Updated imbo {} → {got}  {}",
        crate::VERSION,
        ui::dim(crate::paths::tilde(&exe()))
    ));
    // The new program restarts the daemon and reconnects the tools (ADR-026).
    // Started by the path the user ran, so the configs keep pointing there.
    use std::os::unix::process::CommandExt;
    let err = Command::new(exe()).args(["setup", "--refresh"]).exec();
    bail!("Couldn't restart Imbo with the new program ({err}); run `imbo setup`")
}

fn failed(e: &anyhow::Error) -> anyhow::Error {
    anyhow::anyhow!("Couldn't update: {e:#}. Nothing was changed.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_versions() {
        assert!(newer("0.2.11", "0.2.10"));
        assert!(newer("0.3.0", "0.2.99"));
        assert!(newer("1.0.0", "0.9.9"));
        assert!(!newer("0.2.10", "0.2.10"));
        assert!(!newer("0.2.9", "0.2.10"));
        assert!(newer("0.2.10", "0.2.10-rc.1"));
        assert!(!newer("0.2.10-rc.1", "0.2.10"));
        assert!(!newer("garbage", "0.2.10"));
        assert!(!newer("0.2", "0.1.0"));
        assert!(!newer("0.2.10.1", "0.1.0"));
    }
}
