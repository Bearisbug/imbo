//! `imbo status`.

use imbo_proto::local::{DaemonStatus, LocalRequest};

use super::{Reported, pad, tool_name};
use crate::local::Local;
use crate::paths::Home;
use crate::ui;

pub async fn status() -> anyhow::Result<()> {
    let home = Home::resolve();
    let res = match Local::new().send(&LocalRequest::Status).await {
        Ok(r) => r,
        Err(p) => return Err(super::report(&p)),
    };
    let st: DaemonStatus = serde_json::from_value(res.body)?;
    println!(
        "{}  {}",
        ui::heading("imbo"),
        ui::dim(format!("{} · daemon running, pid {}", st.version, st.pid))
    );
    if let Some(v) = &st.latest_version {
        println!(
            "{} {}  {}",
            ui::s(ui::warn(), "!"),
            ui::s(ui::warn(), format!("imbo {v} is out")),
            ui::dim("run `imbo update`")
        );
    }
    match home.credentials().filter(|_| st.logged_in) {
        Some(c) => println!(
            "{} Signed in as {} {}",
            ui::dot(true),
            ui::heading(&c.name),
            ui::dim(format!("· {}", c.account_id))
        ),
        None => println!(
            "{} {}  {}",
            ui::dot(false),
            ui::s(ui::warn(), "Not signed in"),
            ui::dim("run `imbo login`")
        ),
    }
    if st.connected {
        println!(
            "{} Connected to {}",
            ui::dot(true),
            ui::s(ui::text(), &st.server)
        );
    } else {
        println!(
            "{} {} {}",
            ui::dot(false),
            ui::s(ui::warn(), format!("Not connected to {}", st.server)),
            ui::dim(st.last_error.clone().unwrap_or_default())
        );
    }
    println!();
    if st.sessions.is_empty() {
        println!(
            "{}",
            ui::dim("No agent sessions seen on this computer yet.")
        );
    } else {
        println!("{}", ui::heading("Sessions on this computer"));
        for s in &st.sessions {
            let channel = match s.channel.as_str() {
                "inject" => "live",
                "listen" => "listening",
                "prompt_hook" => "on next prompt",
                "queue" => "queued",
                _ => "no delivery",
            };
            println!(
                "  {} {}  {}  {}",
                ui::dot(s.online),
                ui::s(ui::text(), pad(&s.short_id, 8)),
                pad(&s.title, 26),
                ui::dim(format!(
                    "{} · {}",
                    tool_name(&s.tool),
                    if s.online { channel } else { "offline" }
                ))
            );
        }
    }
    if !st.notices.is_empty() {
        println!();
        println!("{}", ui::heading("Recent notices"));
        for n in st.notices.iter().take(5) {
            println!("  · {}", n.text);
            if let Some(note) = &n.note {
                println!("    {}", ui::s(ui::text(), format!("“{note}”")));
            }
        }
    }
    if !st.logged_in {
        return Err(Reported.into());
    }
    Ok(())
}
