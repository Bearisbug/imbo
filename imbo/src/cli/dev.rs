//! Hidden helpers for tests and demos.

use std::path::PathBuf;

use clap::Subcommand;
use imbo_proto::local::LocalRequest;

use crate::adapters::Install;
use crate::local::Local;
use crate::paths::Home;
use crate::ui;

#[derive(Subcommand)]
pub enum DevCmd {
    /// Write the Claude Code plugin to a directory (for `claude --plugin-dir`).
    WritePlugin { dir: PathBuf },
    /// Connect one tool the way `imbo setup` does, without the daemon
    /// service and sign-in steps.
    Connect { tool: String },
    /// Pin a session with no live tool behind it.
    FakeSession {
        session_id: String,
        #[arg(long)]
        title: String,
        #[arg(long, default_value = imbo_proto::TOOL_CLAUDE)]
        tool: String,
        #[arg(long)]
        remove: bool,
    },
}

pub async fn run(cmd: DevCmd) -> anyhow::Result<()> {
    match cmd {
        DevCmd::WritePlugin { dir } => {
            crate::adapters::claude::write_plugin(&Home::resolve(), &dir)?;
            ui::ok_line(&format!("Plugin written to {}", dir.display()));
        }
        DevCmd::Connect { tool } => {
            let Some(a) = crate::adapters::get(&tool) else {
                anyhow::bail!("no adapter for `{tool}`");
            };
            let Install::Found(bin) = a.find() else {
                anyhow::bail!("{} isn't installed", a.name());
            };
            match a.connect(&Home::resolve(), &bin) {
                Ok(done) => {
                    ui::ok_line(&format!("{} connected  {}", a.name(), done.detail));
                    if let Some(w) = done.warning {
                        ui::warn_line(&w);
                    }
                }
                Err(e) => anyhow::bail!("{}: {e}", a.name()),
            }
        }
        DevCmd::FakeSession {
            session_id,
            title,
            tool,
            remove,
        } => {
            let req = LocalRequest::FakeSession {
                tool,
                session_id,
                title,
                remove,
            };
            if let Err(p) = Local::new().send(&req).await {
                return Err(super::report(&p));
            }
            ui::ok_line(if remove {
                "Session removed"
            } else {
                "Session registered"
            });
        }
    }
    Ok(())
}
