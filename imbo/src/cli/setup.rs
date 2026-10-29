//! `imbo setup` and `imbo uninstall` (FLOW-ONBOARD, REQ-001, REQ-027).

use std::path::PathBuf;
use std::time::Duration;

use clap::Args;
use imbo_proto::local::{DaemonStatus, LocalRequest};

use super::Reported;
use super::login::{choose_server, need_terminal, reload_daemon, sign_in};
use crate::adapters::{self, Adapter, Install};
use crate::local::Local;
use crate::paths::{Home, exe};
use crate::{service, ui};

#[derive(Args)]
pub struct SetupArgs {
    /// Server to use (default https://api.imbo.dev).
    #[arg(long)]
    pub server: Option<String>,
    /// Run by `imbo update` with the new program: restart the daemon and
    /// reconnect the tools the last setup connected, without asking.
    #[arg(long, hide = true)]
    pub refresh: bool,
}

#[derive(Args)]
pub struct UninstallArgs {
    /// Don't ask for confirmation.
    #[arg(long, short)]
    pub yes: bool,
}

/// A step shown with a spinner while it runs. cliclack prints nothing of a
/// spinner, its result line included, when stderr isn't a terminal; there
/// the result is printed as a log line instead.
struct Step(Option<cliclack::ProgressBar>);

impl Step {
    fn start(msg: impl std::fmt::Display) -> Self {
        if !console::Term::stderr().is_term() {
            return Step(None);
        }
        let sp = cliclack::spinner();
        sp.start(msg);
        Step(Some(sp))
    }

    fn done(self, msg: impl std::fmt::Display) {
        match self.0 {
            Some(sp) => sp.stop(msg),
            None => {
                let _ = cliclack::log::success(msg);
            }
        }
    }

    fn fail(self, msg: impl std::fmt::Display) {
        match self.0 {
            Some(sp) => sp.error(msg),
            None => {
                let _ = cliclack::log::error(msg);
            }
        }
    }
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
    if a.refresh {
        return refresh(&home).await;
    }
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
    let sp = Step::start("Starting the background daemon");
    let how = match service::install(&home) {
        Ok(h) => h,
        Err(e) => {
            sp.fail(format!("Couldn't start the daemon: {e:#}"));
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
        Some(s) if s.connected => sp.done(format!(
            "Background daemon running  {}",
            ui::dim(format!("{how} · connected to {}", s.server))
        )),
        Some(s) => sp.done(format!(
            "{}  {}",
            ui::s(ui::warn(), "Background daemon running, not connected yet"),
            ui::dim(s.last_error.clone().unwrap_or_default())
        )),
        None => {
            sp.fail("The background daemon didn't start. See ~/.imbo/logs/daemon.log");
            return Err(Reported.into());
        }
    }

    // 3. Tools. In a terminal the user ticks the ones they use; the list of
    // changes shown next is what they agree to (ADR-013). Otherwise every
    // installed tool is connected.
    let mut chosen: Vec<(&dyn Adapter, PathBuf)> = vec![];
    for a in adapters::all() {
        match a.find() {
            Install::Found(bin) => chosen.push((*a, bin)),
            Install::Missing => cliclack::log::warning(format!(
                "{} not installed, skipped  {}",
                a.name(),
                ui::dim("install it, then run `imbo setup` again")
            ))?,
            Install::Unsupported(why) => {
                cliclack::log::warning(format!("{} skipped  {}", a.name(), ui::dim(why)))?
            }
        }
    }
    if !chosen.is_empty() && console::Term::stderr().is_term() {
        let mut pick = cliclack::multiselect("Which tools do you use Imbo in?").required(false);
        for (a, _) in &chosen {
            pick = pick.item(a.id(), a.name(), "");
        }
        let ticked = chosen.iter().map(|(a, _)| a.id()).collect();
        let picked = pick.initial_values(ticked).interact()?;
        chosen.retain(|(a, _)| picked.contains(&a.id()));
    }
    let changes: Vec<String> = chosen.iter().map(|(a, _)| a.changes()).collect();
    if !changes.is_empty() {
        cliclack::log::info(format!(
            "Changing these so Imbo's tools run without asking each time:\n{}",
            changes.join("\n")
        ))?;
    }
    let mut connected = vec![];
    for (a, bin) in &chosen {
        let sp = Step::start(format!("Connecting {}", a.name()));
        match a.connect(&home, bin) {
            Ok(done) => {
                connected.push(a.id());
                sp.done(format!("{} connected  {}", a.name(), ui::dim(done.detail)));
                if let Some(w) = done.warning {
                    cliclack::log::warning(w)?;
                }
                if let Some(note) = a.after_connect(&home, console::Term::stderr().is_term())? {
                    cliclack::log::remark(note)?;
                }
            }
            Err(e) => sp.fail(format!("{}: {e}", a.name())),
        }
    }
    save_tools(&home, &connected);
    if !connected.is_empty() {
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
        let names: Vec<&str> = adapters::all().iter().map(|a| a.name()).collect();
        let go: bool = cliclack::confirm(format!(
            "Disconnect {}, stop the daemon and sign this computer out?",
            and_list(&names)
        ))
        .initial_value(true)
        .interact()?;
        if !go {
            return Ok(());
        }
    }
    for a in adapters::all() {
        let bin = match a.find() {
            Install::Found(b) => Some(b),
            _ => None,
        };
        a.disconnect(&home, bin.as_deref());
        if bin.is_some() {
            ui::ok_line(&format!("{} disconnected", a.name()));
        }
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
    let _ = std::fs::remove_file(tools_path(&home));
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

/// The tools the last setup connected (ADR-026): `imbo update` reconnects these.
fn tools_path(home: &Home) -> PathBuf {
    home.state().join("tools")
}

fn save_tools(home: &Home, ids: &[&str]) {
    let _ = std::fs::write(
        tools_path(home),
        ids.iter().map(|i| format!("{i}\n")).collect::<String>(),
    );
}

fn saved_tools(home: &Home) -> Vec<String> {
    std::fs::read_to_string(tools_path(home))
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
        .collect()
}

/// `imbo setup --refresh`, run by the new program after `imbo update`.
async fn refresh(home: &Home) -> anyhow::Result<()> {
    if home.credentials().is_none() {
        ui::ok_line("Not signed in on this computer · run `imbo setup` to sign in");
        return Ok(());
    }
    let how = match service::install(home) {
        Ok(h) => h,
        Err(e) => {
            ui::error_line(&format!("Couldn't restart the daemon: {e:#}"));
            return Err(Reported.into());
        }
    };
    ui::ok_line(&format!("Background daemon restarted  {}", ui::dim(how)));
    let mut reconnected = false;
    for id in saved_tools(home) {
        let Some(a) = adapters::get(&id) else {
            continue;
        };
        match a.find() {
            Install::Found(bin) => match a.connect(home, &bin) {
                Ok(done) => {
                    reconnected = true;
                    ui::ok_line(&format!(
                        "{} reconnected  {}",
                        a.name(),
                        ui::dim(done.detail)
                    ));
                    if let Some(w) = done.warning {
                        ui::warn_line(&w);
                    }
                }
                Err(e) => ui::error_line(&format!("{}: {e}", a.name())),
            },
            _ => ui::warn_line(&format!("{} is no longer installed, skipped", a.name())),
        }
    }
    if reconnected {
        eprintln!(
            "  {}",
            ui::dim("Sessions that were already open use the new version after a restart.")
        );
    }
    Ok(())
}

/// "A", "A and B", "A, B and C".
fn and_list(names: &[&str]) -> String {
    match names.split_last() {
        Some((last, [])) => last.to_string(),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
        None => String::new(),
    }
}
