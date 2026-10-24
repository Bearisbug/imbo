//! `imbo logout | whoami | rename | device …`

use clap::Subcommand;
use imbo_proto::api::{AccountResponse, DevicesResponse, MeResponse};
use imbo_proto::text::validate_name;
use serde_json::json;

use super::login::reload_daemon;
use super::{Reported, api, api_as, pad};
use crate::local::Local;
use crate::paths::Home;
use crate::ui;

#[derive(Subcommand)]
pub enum DeviceCmd {
    /// List computers signed in to your account.
    List,
    /// Sign another computer out (ID from `imbo device list`).
    Revoke {
        id: String,
        #[arg(long, short)]
        yes: bool,
    },
}

pub async fn logout() -> anyhow::Result<()> {
    let home = Home::resolve();
    if home.credentials().is_none() {
        ui::ok_line("This computer isn't signed in.");
        return Ok(());
    }
    if let Err(p) = Local::new()
        .call("POST", "/v1/auth/logout", None, None)
        .await
    {
        ui::warn_line(&format!(
            "Couldn't reach the server ({}); signed out on this computer only",
            p.title
        ));
    }
    home.clear_credentials();
    reload_daemon().await;
    ui::ok_line("Signed out. Sign in again with `imbo login`.");
    Ok(())
}

pub async fn whoami() -> anyhow::Result<()> {
    let me: MeResponse = api_as("GET", "/v1/me", None).await?;
    let server = Home::resolve()
        .credentials()
        .map(|c| c.server)
        .unwrap_or_default();
    println!(
        "{}  {}",
        ui::heading(&me.account.name),
        ui::s(ui::ok(), me.account.id)
    );
    if let Some(e) = &me.account.email {
        println!("{}", ui::dim(format!("email     {e}")));
    }
    println!("{}", ui::dim(format!("computer  {}", me.device.name)));
    println!("{}", ui::dim(format!("server    {server}")));
    Ok(())
}

pub async fn rename(name: String) -> anyhow::Result<()> {
    let name = match validate_name(&name) {
        Ok(n) => n,
        Err(e) => {
            ui::error_line(e);
            return Err(Reported.into());
        }
    };
    let r: AccountResponse = api_as("PATCH", "/v1/me", Some(json!({ "name": name }))).await?;
    let home = Home::resolve();
    if let Some(mut c) = home.credentials() {
        c.name = r.account.name.clone();
        let _ = home.save_credentials(&c);
    }
    ui::ok_line(&format!("You're now {}", ui::heading(&r.account.name)));
    Ok(())
}

pub async fn device(cmd: DeviceCmd) -> anyhow::Result<()> {
    let r: DevicesResponse = api_as("GET", "/v1/devices", None).await?;
    match cmd {
        DeviceCmd::List => {
            for d in &r.devices {
                let seen = if d.current {
                    ui::s(ui::ok(), "this computer").to_string()
                } else {
                    ui::dim(
                        d.last_seen_at
                            .map(|t| format!("seen {}", ui::ago(t)))
                            .unwrap_or_else(|| "never seen".into()),
                    )
                };
                println!(
                    "{} {}  {}  {}  {}",
                    ui::dot(d.current),
                    pad(&ui::heading(&d.name), 20),
                    ui::dim(pad(&d.platform, 14)),
                    pad(&seen, 16),
                    ui::dim(&d.id.to_string()[28..])
                );
            }
            Ok(())
        }
        DeviceCmd::Revoke { id, yes } => {
            let hits: Vec<_> = r
                .devices
                .iter()
                .filter(|d| d.id.to_string().ends_with(id.trim()))
                .collect();
            let d = match hits.as_slice() {
                [one] => *one,
                [] => {
                    ui::error_line(&format!(
                        "No computer with ID {id}. See `imbo device list`."
                    ));
                    return Err(Reported.into());
                }
                _ => {
                    ui::error_line("More than one computer matches; give the full ID.");
                    return Err(Reported.into());
                }
            };
            if !yes && ui::is_interactive() {
                let what = if d.current {
                    "this computer"
                } else {
                    d.name.as_str()
                };
                let go: bool = cliclack::confirm(format!("Sign {what} out of Imbo?"))
                    .initial_value(false)
                    .interact()?;
                if !go {
                    return Ok(());
                }
            }
            api("DELETE", &format!("/v1/devices/{}", d.id), None).await?;
            ui::ok_line(&format!(
                "Signed {} out. Its sessions are offline now.",
                ui::heading(&d.name)
            ));
            Ok(())
        }
    }
}
