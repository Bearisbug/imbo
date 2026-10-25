//! `imbo setup` and `imbo uninstall` (FLOW-ONBOARD, REQ-001, REQ-027).

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use clap::Args;
use imbo_proto::local::{DaemonStatus, LocalRequest};
use serde_json::{Value, json};

use super::Reported;
use super::login::{choose_server, need_terminal, reload_daemon, sign_in};
use crate::local::Local;
use crate::paths::{Home, claude_dir, codex_home, user_home, which};
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

/// Claude Code permission rule covering every tool of the imbo plugin's MCP server.
const CLAUDE_RULE: &str = "mcp__plugin_imbo_imbo";

fn claude_settings() -> PathBuf {
    claude_dir().join("settings.json")
}

fn codex_config() -> PathBuf {
    codex_home().join("config.toml")
}

/// `p` with the home directory written as `~`.
fn tilde(p: &Path) -> String {
    match p.strip_prefix(user_home()) {
        Ok(rest) => format!("~/{}", rest.display()),
        Err(_) => p.display().to_string(),
    }
}

/// Claude Code settings text with `CLAUDE_RULE` added to (or removed from)
/// `permissions.allow`, everything else kept; `None` when nothing changes.
fn edit_claude_allow(text: &str, add: bool) -> Result<Option<String>, String> {
    let mut v: Value = if text.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(text).map_err(|e| format!("not valid JSON: {e}"))?
    };
    let root = v.as_object_mut().ok_or("not a JSON object")?;
    if !add && !root.contains_key("permissions") {
        return Ok(None);
    }
    let perms = root
        .entry("permissions")
        .or_insert_with(|| json!({}))
        .as_object_mut()
        .ok_or("`permissions` isn't an object")?;
    if !add && !perms.contains_key("allow") {
        return Ok(None);
    }
    let allow = perms
        .entry("allow")
        .or_insert_with(|| json!([]))
        .as_array_mut()
        .ok_or("`permissions.allow` isn't a list")?;
    match (add, allow.iter().any(|r| r == CLAUDE_RULE)) {
        (true, false) => allow.push(json!(CLAUDE_RULE)),
        (false, true) => allow.retain(|r| r != CLAUDE_RULE),
        _ => return Ok(None),
    }
    let mut out = serde_json::to_string_pretty(&v).map_err(|e| e.to_string())?;
    out.push('\n');
    Ok(Some(out))
}

/// Let Claude Code run Imbo's tools without asking each time (ADR-013), or
/// take that back.
fn allow_claude_tools(add: bool) -> Result<(), String> {
    let path = claude_settings();
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(e.to_string()),
    };
    if let Some(new) = edit_claude_allow(&text, add)? {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        }
        std::fs::write(&path, new).map_err(|e| e.to_string())?;
    }
    Ok(())
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
    let config = codex_config();
    let text = std::fs::read_to_string(&config).map_err(|e| e.to_string())?;
    let with_keys = add_codex_keys(&text);
    if with_keys != text {
        std::fs::write(&config, with_keys).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Keys `codex mcp add` has no flags for: uploads can outlast Codex's default
/// 60 s tool timeout (API-MCP-001), and Imbo's tools run without asking each
/// time (ADR-013).
const CODEX_KEYS: [(&str, &str); 2] = [
    ("tool_timeout_sec", "600"),
    ("default_tools_approval_mode", "\"approve\""),
];

fn codex_keys_text() -> String {
    CODEX_KEYS
        .iter()
        .map(|(k, v)| format!("{k} = {v}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Put the missing `CODEX_KEYS` into the `[mcp_servers.imbo]` table that
/// `codex mcp add` wrote.
fn add_codex_keys(config: &str) -> String {
    let lines: Vec<&str> = config.lines().collect();
    let Some(start) = lines.iter().position(|l| l.trim() == "[mcp_servers.imbo]") else {
        return config.to_string();
    };
    let end = lines[start + 1..]
        .iter()
        .position(|l| l.trim_start().starts_with('['))
        .map_or(lines.len(), |i| start + 1 + i);
    let missing: Vec<String> = CODEX_KEYS
        .iter()
        .filter(|(k, _)| {
            !lines[start + 1..end]
                .iter()
                .any(|l| l.trim_start().starts_with(k))
        })
        .map(|(k, v)| format!("{k} = {v}"))
        .collect();
    if missing.is_empty() {
        return config.to_string();
    }
    let mut out = lines[..=start].to_vec();
    out.extend(missing.iter().map(String::as_str));
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

    // 3. Tools. In a terminal the user ticks the ones they use; the list of
    // changes shown next is what they agree to (ADR-013). Otherwise every
    // installed tool is connected.
    let claude = which("claude");
    let codex = which("codex");
    for (name, found) in [("Claude Code", &claude), ("Codex", &codex)] {
        if found.is_none() {
            cliclack::log::warning(format!(
                "{name} not installed, skipped  {}",
                ui::dim("install it, then run `imbo setup` again")
            ))?;
        }
    }
    let (mut use_claude, mut use_codex) = (claude.is_some(), codex.is_some());
    if (use_claude || use_codex) && console::Term::stderr().is_term() {
        let mut pick = cliclack::multiselect("Which tools do you use Imbo in?").required(false);
        let mut ticked = vec![];
        if use_claude {
            pick = pick.item("claude", "Claude Code", "");
            ticked.push("claude");
        }
        if use_codex {
            pick = pick.item("codex", "Codex", "");
            ticked.push("codex");
        }
        let chosen = pick.initial_values(ticked).interact()?;
        use_claude = chosen.contains(&"claude");
        use_codex = chosen.contains(&"codex");
    }
    let mut changes = vec![];
    if use_claude {
        changes.push(format!(
            "Claude Code  {}  plugin imbo; {CLAUDE_RULE} in permissions.allow",
            tilde(&claude_settings())
        ));
    }
    if use_codex {
        changes.push(format!(
            "Codex  {}  [mcp_servers.imbo] with {}",
            tilde(&codex_config()),
            codex_keys_text()
        ));
    }
    if !changes.is_empty() {
        cliclack::log::info(format!(
            "Changing these so Imbo's tools run without asking each time:\n{}",
            changes.join("\n")
        ))?;
    }
    let mut connected_any = false;
    if let Some(claude) = claude.filter(|_| use_claude) {
        let sp = cliclack::spinner();
        sp.start("Connecting Claude Code");
        match connect_claude(&home, &claude) {
            Ok(()) => {
                connected_any = true;
                sp.stop(format!(
                    "Claude Code connected  {}",
                    ui::dim("plugin imbo: tools + prompt hook")
                ));
                if let Err(e) = allow_claude_tools(true) {
                    cliclack::log::warning(format!(
                        "Couldn't edit {} ({e})  {}",
                        tilde(&claude_settings()),
                        ui::dim(format!(
                            "add \"{CLAUDE_RULE}\" to permissions.allow there, or Claude Code asks before each Imbo tool"
                        ))
                    ))?;
                }
            }
            Err(e) => sp.error(format!("Claude Code: {e}")),
        }
    }
    if let Some(codex) = codex.filter(|_| use_codex) {
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
    let _ = allow_claude_tools(false);
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
    fn codex_keys_go_into_the_imbo_table() {
        let before = "[mcp_servers.other]\ntool_timeout_sec = 30\n\n[mcp_servers.imbo]\ncommand = \"/bin/imbo\"\nargs = [\"mcp\"]\n";
        let after = add_codex_keys(before);
        assert_eq!(
            after,
            "[mcp_servers.other]\ntool_timeout_sec = 30\n\n[mcp_servers.imbo]\ntool_timeout_sec = 600\ndefault_tools_approval_mode = \"approve\"\ncommand = \"/bin/imbo\"\nargs = [\"mcp\"]\n"
        );
        assert_eq!(add_codex_keys(&after), after);
        assert_eq!(add_codex_keys("model = \"x\"\n"), "model = \"x\"\n");
    }

    #[test]
    fn claude_allow_rule_is_added_once_and_removed() {
        let before =
            r#"{"model":"sonnet","permissions":{"allow":["Bash(ls:*)"],"deny":[]},"hooks":{}}"#;
        let added = edit_claude_allow(before, true).unwrap().unwrap();
        let v: Value = serde_json::from_str(&added).unwrap();
        assert_eq!(
            v["permissions"]["allow"],
            json!(["Bash(ls:*)", "mcp__plugin_imbo_imbo"])
        );
        let keys: Vec<_> = v.as_object().unwrap().keys().cloned().collect();
        assert_eq!(keys, ["model", "permissions", "hooks"]);
        assert_eq!(edit_claude_allow(&added, true).unwrap(), None);

        let removed = edit_claude_allow(&added, false).unwrap().unwrap();
        let v: Value = serde_json::from_str(&removed).unwrap();
        assert_eq!(v, serde_json::from_str::<Value>(before).unwrap());
        assert_eq!(edit_claude_allow(&removed, false).unwrap(), None);

        let fresh = edit_claude_allow("", true).unwrap().unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&fresh).unwrap(),
            json!({ "permissions": { "allow": ["mcp__plugin_imbo_imbo"] } })
        );
        assert_eq!(edit_claude_allow("", false).unwrap(), None);
        assert!(edit_claude_allow("{ not json", true).is_err());
    }
}
