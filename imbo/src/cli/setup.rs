//! `imbo setup` and `imbo uninstall` (FLOW-ONBOARD, REQ-001, REQ-027).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use clap::Args;
use imbo_proto::local::{DaemonStatus, LocalRequest};
use serde_json::json;

use super::Reported;
use super::login::{choose_server, need_terminal, reload_daemon, sign_in};
use crate::local::Local;
use crate::paths::{Home, which};
use crate::{service, ui};

#[derive(Args)]
pub struct SetupArgs {
    /// Server to use (default https://api.imbo.dev).
    #[arg(long)]
    pub server: Option<String>,
}

#[derive(Args)]
pub struct UninstallArgs {
    /// Don't ask for confirmation.
    #[arg(long, short)]
    pub yes: bool,
}

fn exe() -> PathBuf {
    std::env::current_exe().unwrap_or_else(|_| PathBuf::from("imbo"))
}

fn sh_quote(s: &str) -> String {
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-+:=@".contains(c))
    {
        s.to_string()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// Write a Claude Code plugin (MCP server + prompt hook) at `root`.
pub fn write_plugin(home: &Home, root: &Path) -> anyhow::Result<()> {
    let exe = exe().to_string_lossy().into_owned();
    std::fs::create_dir_all(root.join(".claude-plugin"))?;
    std::fs::create_dir_all(root.join("hooks"))?;
    let manifest = json!({
        "name": "imbo",
        "version": crate::VERSION,
        "description": "Messages between Claude Code and Codex sessions, across people and computers.",
        "author": { "name": "Imbo" },
        "homepage": "https://imbo.dev",
        "license": "Apache-2.0"
    });
    std::fs::write(
        root.join(".claude-plugin/plugin.json"),
        serde_json::to_string_pretty(&manifest)?,
    )?;
    let mut server = json!({ "command": exe, "args": ["mcp"] });
    let mut hook_cmd = format!("{} hook prompt", sh_quote(&exe));
    if !home.is_primary() {
        let h = home.dir.to_string_lossy().into_owned();
        server["env"] = json!({ "IMBO_HOME": h });
        hook_cmd = format!("IMBO_HOME={} {hook_cmd}", sh_quote(&h));
    }
    let mcp = json!({ "mcpServers": { "imbo": server } });
    std::fs::write(root.join(".mcp.json"), serde_json::to_string_pretty(&mcp)?)?;
    let hooks = json!({
        "hooks": {
            "UserPromptSubmit": [
                { "hooks": [ { "type": "command", "command": hook_cmd, "timeout": 10 } ] }
            ]
        }
    });
    std::fs::write(
        root.join("hooks/hooks.json"),
        serde_json::to_string_pretty(&hooks)?,
    )?;
    Ok(())
}

/// The local marketplace `imbo setup` registers with Claude Code.
fn write_marketplace(home: &Home) -> anyhow::Result<PathBuf> {
    let dir = home.plugin_dir();
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join(".claude-plugin"))?;
    let market = json!({
        "name": marketplace_name(home),
        "owner": { "name": "Imbo" },
        "plugins": [ {
            "name": "imbo",
            "source": "./plugin",
            "description": "Messages between Claude Code and Codex sessions, across people and computers.",
            "version": crate::VERSION
        } ]
    });
    std::fs::write(
        dir.join(".claude-plugin/marketplace.json"),
        serde_json::to_string_pretty(&market)?,
    )?;
    write_plugin(home, &dir.join("plugin"))?;
    Ok(dir)
}

fn marketplace_name(home: &Home) -> String {
    if home.is_primary() {
        "imbo".into()
    } else {
        home.label().replace("dev.imbo.daemon.", "imbo-")
    }
}

fn run(cmd: &Path, args: &[&str]) -> Result<String, String> {
    match Command::new(cmd)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
    {
        Ok(o) if o.status.success() => Ok(String::from_utf8_lossy(&o.stdout).into_owned()),
        Ok(o) => {
            let mut e = String::from_utf8_lossy(&o.stderr).trim().to_string();
            if e.is_empty() {
                e = String::from_utf8_lossy(&o.stdout).trim().to_string();
            }
            Err(e.lines().last().unwrap_or("failed").to_string())
        }
        Err(e) => Err(e.to_string()),
    }
}

fn connect_claude(home: &Home, claude: &Path) -> Result<(), String> {
    let dir = write_marketplace(home).map_err(|e| e.to_string())?;
    let market = marketplace_name(home);
    let _ = run(claude, &["plugin", "marketplace", "remove", &market]);
    run(
        claude,
        &["plugin", "marketplace", "add", &dir.to_string_lossy()],
    )?;
    run(
        claude,
        &[
            "plugin",
            "install",
            &format!("imbo@{market}"),
            "--scope",
            "user",
        ],
    )?;
    Ok(())
}

fn disconnect_claude(home: &Home, claude: &Path) {
    let market = marketplace_name(home);
    let _ = run(
        claude,
        &[
            "plugin",
            "uninstall",
            &format!("imbo@{market}"),
            "--scope",
            "user",
        ],
    );
    let _ = run(claude, &["plugin", "marketplace", "remove", &market]);
    let _ = std::fs::remove_dir_all(home.plugin_dir());
}

fn connect_codex(home: &Home, codex: &Path) -> Result<(), String> {
    let _ = run(codex, &["mcp", "remove", "imbo"]);
    let exe = exe().to_string_lossy().into_owned();
    let env = format!("IMBO_HOME={}", home.dir.display());
    let mut args = vec!["mcp", "add", "imbo"];
    if !home.is_primary() {
        args.extend(["--env", env.as_str()]);
    }
    args.extend(["--", exe.as_str(), "mcp"]);
    run(codex, &args)?;
    // Uploads can outlast Codex's default 60 s tool timeout (API-MCP-001).
    let config = crate::paths::codex_home().join("config.toml");
    let text = std::fs::read_to_string(&config).map_err(|e| e.to_string())?;
    let with_timeout = add_tool_timeout(&text);
    if with_timeout != text {
        std::fs::write(&config, with_timeout).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Put `tool_timeout_sec = 600` into the `[mcp_servers.imbo]` table that
/// `codex mcp add` wrote; there's no flag for it.
fn add_tool_timeout(config: &str) -> String {
    let lines: Vec<&str> = config.lines().collect();
    let Some(start) = lines.iter().position(|l| l.trim() == "[mcp_servers.imbo]") else {
        return config.to_string();
    };
    let end = lines[start + 1..]
        .iter()
        .position(|l| l.trim_start().starts_with('['))
        .map_or(lines.len(), |i| start + 1 + i);
    if lines[start + 1..end]
        .iter()
        .any(|l| l.trim_start().starts_with("tool_timeout_sec"))
    {
        return config.to_string();
    }
    let mut out = lines[..=start].to_vec();
    out.push("tool_timeout_sec = 600");
    out.extend(&lines[start + 1..]);
    let mut s = out.join("\n");
    if config.ends_with('\n') {
        s.push('\n');
    }
    s
}

async fn daemon_status() -> Option<DaemonStatus> {
    let res = Local::new()
        .send_with(&LocalRequest::Status, Duration::from_secs(3))
        .await
        .ok()?;
    serde_json::from_value(res.body).ok()
}

pub async fn setup(a: SetupArgs) -> anyhow::Result<()> {
    let home = Home::resolve();
    home.ensure()?;
    cliclack::intro(ui::heading("imbo setup"))?;
    let server = choose_server(&home, a.server)?;

    // 1. Sign in, unless this computer already is (on the same server).
    let creds = match home.credentials().filter(|c| c.server == server) {
        Some(c) => {
            cliclack::log::success(format!(
                "Signed in as {} {}  {}",
                ui::heading(&c.name),
                ui::dim(format!("· {}", c.account_id)),
                ui::dim("skipping sign-in")
            ))?;
            c
        }
        None => {
            need_terminal("imbo setup")?;
            sign_in(&home, &server, None).await?
        }
    };

    // 2. Background daemon.
    let sp = cliclack::spinner();
    sp.start("Starting the background daemon");
    let how = match service::install(&home) {
        Ok(h) => h,
        Err(e) => {
            sp.error(format!("Couldn't start the daemon: {e:#}"));
            return Err(Reported.into());
        }
    };
    reload_daemon().await;
    let mut st = None;
    for _ in 0..40 {
        st = daemon_status().await;
        if st.as_ref().is_some_and(|s| s.connected) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    match &st {
        Some(s) if s.connected => sp.stop(format!(
            "Background daemon running  {}",
            ui::dim(format!("{how} · connected to {}", s.server))
        )),
        Some(s) => sp.stop(format!(
            "{}  {}",
            ui::s(ui::warn(), "Background daemon running, not connected yet"),
            ui::dim(s.last_error.clone().unwrap_or_default())
        )),
        None => {
            sp.error("The background daemon didn't start. See ~/.imbo/logs/daemon.log");
            return Err(Reported.into());
        }
    }

    // 3. Tools.
    let mut connected_any = false;
    match which("claude") {
        Some(claude) => {
            let sp = cliclack::spinner();
            sp.start("Connecting Claude Code");
            match connect_claude(&home, &claude) {
                Ok(()) => {
                    connected_any = true;
                    sp.stop(format!(
                        "Claude Code connected  {}",
                        ui::dim("plugin imbo: tools + prompt hook")
                    ));
                }
                Err(e) => sp.error(format!("Claude Code: {e}")),
            }
        }
        None => cliclack::log::warning(format!(
            "Claude Code not installed, skipped  {}",
            ui::dim("install it, then run `imbo setup` again")
        ))?,
    }
    match which("codex") {
        Some(codex) => {
            let sp = cliclack::spinner();
            sp.start("Connecting Codex");
            match connect_codex(&home, &codex) {
                Ok(()) => {
                    connected_any = true;
                    sp.stop(format!("Codex connected  {}", ui::dim("MCP server imbo")));
                }
                Err(e) => sp.error(format!("Codex: {e}")),
            }
        }
        None => cliclack::log::warning(format!(
            "Codex not installed, skipped  {}",
            ui::dim("install it, then run `imbo setup` again")
        ))?,
    }
    if connected_any {
        cliclack::log::remark(ui::dim(
            "Sessions that were already open pick Imbo up after a restart.",
        ))?;
    }
    cliclack::outro(format!(
        "You're {} · account ID {}  {}",
        ui::heading(&creds.name),
        ui::s(ui::ok(), creds.account_id),
        ui::dim("share the ID so people can add you · `imbo whoami`")
    ))?;
    Ok(())
}

pub async fn uninstall(a: UninstallArgs) -> anyhow::Result<()> {
    let home = Home::resolve();
    if !a.yes && ui::is_interactive() {
        let go: bool = cliclack::confirm(
            "Disconnect Claude Code and Codex, stop the daemon and sign this computer out?",
        )
        .initial_value(true)
        .interact()?;
        if !go {
            return Ok(());
        }
    }
    if let Some(claude) = which("claude") {
        disconnect_claude(&home, &claude);
        ui::ok_line("Claude Code disconnected");
    }
    if let Some(codex) = which("codex") {
        let _ = run(&codex, &["mcp", "remove", "imbo"]);
        ui::ok_line("Codex disconnected");
    }
    if home.credentials().is_some() {
        match Local::new()
            .call("POST", "/v1/auth/logout", None, None)
            .await
        {
            Ok(_) => ui::ok_line("Signed out on the server"),
            Err(p) => ui::warn_line(&format!(
                "Couldn't sign out on the server ({}); signed out locally",
                p.title
            )),
        }
        home.clear_credentials();
    }
    service::uninstall(&home);
    let _ = std::fs::remove_file(home.socket());
    ui::ok_line("Background daemon stopped and removed");
    let bin = exe();
    eprintln!(
        "  {}",
        ui::dim(format!(
            "To delete the program too: rm {} && rm -rf {}",
            bin.display(),
            home.dir.display()
        ))
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_tool_timeout_goes_into_the_imbo_table() {
        let before = "[mcp_servers.other]\ntool_timeout_sec = 30\n\n[mcp_servers.imbo]\ncommand = \"/bin/imbo\"\nargs = [\"mcp\"]\n";
        let after = add_tool_timeout(before);
        assert_eq!(
            after,
            "[mcp_servers.other]\ntool_timeout_sec = 30\n\n[mcp_servers.imbo]\ntool_timeout_sec = 600\ncommand = \"/bin/imbo\"\nargs = [\"mcp\"]\n"
        );
        assert_eq!(add_tool_timeout(&after), after);
        assert_eq!(add_tool_timeout("model = \"x\"\n"), "model = \"x\"\n");
    }
}
