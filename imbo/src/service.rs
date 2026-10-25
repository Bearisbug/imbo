//! Keeping `imbo daemon` running: a launchd agent on macOS, a systemd user
//! service on Linux, or a detached process when neither is available.

use std::path::PathBuf;
use std::process::{Command, Stdio};

use anyhow::{Context, bail};

use crate::adapters;
use crate::paths::{Home, user_home};

fn exe() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("imbo"))
}

fn uid() -> u32 {
    unsafe { libc::getuid() }
}

fn plist_path(home: &Home) -> PathBuf {
    user_home()
        .join("Library/LaunchAgents")
        .join(format!("{}.plist", home.label()))
}

fn unit_path(home: &Home) -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| user_home().join(".config"));
    base.join("systemd/user")
        .join(format!("{}.service", home.label()))
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Environment the daemon needs: where its home is, and a PATH that finds
/// `codex` for `codex queue`.
fn daemon_env(home: &Home) -> Vec<(String, String)> {
    let mut env = vec![];
    if !home.is_primary() {
        env.push(("IMBO_HOME".into(), home.dir.to_string_lossy().into_owned()));
    }
    let mut path = std::env::var("PATH").unwrap_or_default();
    for extra in ["/usr/local/bin", "/opt/homebrew/bin", "/usr/bin", "/bin"] {
        if !path.split(':').any(|p| p == extra) {
            path.push(':');
            path.push_str(extra);
        }
    }
    env.push(("PATH".into(), path));
    env.push(("HOME".into(), user_home().to_string_lossy().into_owned()));
    let tools = adapters::all()
        .iter()
        .flat_map(|a| a.env_pass().iter().copied());
    for k in tools.chain(["IMBO_FAULT_INJECT"]) {
        if let Ok(v) = std::env::var(k)
            && !v.is_empty()
        {
            env.push((k.into(), v));
        }
    }
    env
}

pub fn installed(home: &Home) -> bool {
    if cfg!(target_os = "macos") {
        plist_path(home).exists()
    } else {
        unit_path(home).exists()
    }
}

/// Write and (re)start the service. Returns a description of what runs it.
pub fn install(home: &Home) -> anyhow::Result<&'static str> {
    home.ensure()?;
    if cfg!(target_os = "macos") {
        let plist = plist_path(home);
        std::fs::create_dir_all(plist.parent().unwrap())?;
        let env: String = daemon_env(home)
            .iter()
            .map(|(k, v)| {
                format!(
                    "    <key>{}</key><string>{}</string>\n",
                    xml_escape(k),
                    xml_escape(v)
                )
            })
            .collect();
        let log = home.logs().join("daemon.log");
        let body = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>{label}</string>
  <key>ProgramArguments</key>
  <array><string>{exe}</string><string>daemon</string></array>
  <key>EnvironmentVariables</key>
  <dict>
{env}  </dict>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
  <key>ThrottleInterval</key><integer>5</integer>
  <key>ProcessType</key><string>Background</string>
  <key>StandardOutPath</key><string>{log}</string>
  <key>StandardErrorPath</key><string>{log}</string>
</dict>
</plist>
"#,
            label = xml_escape(&home.label()),
            exe = xml_escape(&exe().to_string_lossy()),
            log = xml_escape(&log.to_string_lossy()),
        );
        std::fs::write(&plist, body)?;
        let target = format!("gui/{}", uid());
        let _ = Command::new("launchctl")
            .args(["bootout", &format!("{target}/{}", home.label())])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        stop_running(home);
        let out = Command::new("launchctl")
            .args(["bootstrap", &target, &plist.to_string_lossy()])
            .output()
            .context("launchctl bootstrap")?;
        if !out.status.success() {
            bail!(
                "launchctl bootstrap failed: {}",
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        Ok("launchd")
    } else {
        let unit = unit_path(home);
        if Command::new("systemctl")
            .args(["--user", "show-environment"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
        {
            std::fs::create_dir_all(unit.parent().unwrap())?;
            let env: String = daemon_env(home)
                .iter()
                .map(|(k, v)| format!("Environment=\"{k}={v}\"\n"))
                .collect();
            let body = format!(
                "[Unit]\nDescription=Imbo daemon\nAfter=network-online.target\n\n[Service]\nExecStart={} daemon\n{env}Restart=on-failure\nRestartSec=5\n\n[Install]\nWantedBy=default.target\n",
                exe().display()
            );
            std::fs::write(&unit, body)?;
            let label = home.label();
            run_ok("systemctl", &["--user", "daemon-reload"])?;
            stop_running(home);
            run_ok("systemctl", &["--user", "enable", &label])?;
            run_ok("systemctl", &["--user", "restart", &label])?;
            Ok("systemd")
        } else {
            stop_running(home);
            spawn_detached(home)?;
            Ok("background process (no systemd user session; it will not start after reboot)")
        }
    }
}

fn run_ok(cmd: &str, args: &[&str]) -> anyhow::Result<()> {
    let out = Command::new(cmd)
        .args(args)
        .output()
        .with_context(|| format!("run {cmd}"))?;
    if !out.status.success() {
        bail!(
            "{cmd} {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(())
}

pub fn uninstall(home: &Home) {
    if cfg!(target_os = "macos") {
        let _ = Command::new("launchctl")
            .args(["bootout", &format!("gui/{}/{}", uid(), home.label())])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let _ = std::fs::remove_file(plist_path(home));
    } else {
        let label = home.label();
        let _ = Command::new("systemctl")
            .args(["--user", "disable", "--now", &label])
            .output();
        let _ = std::fs::remove_file(unit_path(home));
        let _ = Command::new("systemctl")
            .args(["--user", "daemon-reload"])
            .output();
    }
    stop_running(home);
}

/// Ask a running daemon (if any) to exit and wait briefly for it.
fn stop_running(home: &Home) {
    let pid_file = home.dir.join("daemon.pid");
    if let Some(pid) = std::fs::read_to_string(&pid_file)
        .ok()
        .and_then(|s| s.trim().parse::<i32>().ok())
    {
        unsafe {
            if libc::kill(pid, 0) == 0 {
                libc::kill(pid, libc::SIGTERM);
                for _ in 0..30 {
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    if libc::kill(pid, 0) != 0 {
                        break;
                    }
                }
            }
        }
    }
}

fn spawn_detached(home: &Home) -> anyhow::Result<()> {
    use std::os::unix::process::CommandExt;
    home.ensure()?;
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(home.logs().join("daemon.log"))?;
    let mut cmd = Command::new(exe());
    cmd.arg("daemon")
        .env("IMBO_HOME", &home.dir)
        .stdin(Stdio::null())
        .stdout(log.try_clone()?)
        .stderr(log);
    // Never let a daemon inherit an agent session's identity.
    for k in adapters::all().iter().flat_map(|a| a.env_session()) {
        cmd.env_remove(k);
    }
    unsafe {
        cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }
    cmd.spawn().context("start imbo daemon")?;
    Ok(())
}

/// Start the daemon through the service manager if installed, otherwise as
/// a detached process. Errors are ignored; the caller retries the socket.
pub fn start_daemon(home: &Home) {
    if installed(home) {
        if cfg!(target_os = "macos") {
            let _ = Command::new("launchctl")
                .args(["kickstart", &format!("gui/{}/{}", uid(), home.label())])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status();
        } else {
            let _ = Command::new("systemctl")
                .args(["--user", "start", &home.label()])
                .output();
        }
        return;
    }
    let _ = spawn_detached(home);
}
