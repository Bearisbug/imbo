//! Terminal commands. Everything that needs the server goes through the
//! daemon (API-LOCAL-002); login talks to the server directly.

pub mod account;
pub mod contact;
pub mod dev;
pub mod group;
pub mod hook;
pub mod login;
mod messages;
pub mod setup;
mod status;
mod wait;

use imbo_proto::Problem;
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::local::{Local, env_actor};
use crate::ui;

/// An error already shown to the user; `main` only sets the exit code.
#[derive(Debug)]
pub struct Reported;

impl std::fmt::Display for Reported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("reported")
    }
}

impl std::error::Error for Reported {}

pub async fn dispatch(cmd: crate::Cmd) -> anyhow::Result<()> {
    use crate::Cmd;
    // The new-version hint (REQ-046); status shows it in its own output.
    let hint = !matches!(
        cmd,
        Cmd::Setup(_) | Cmd::Uninstall(_) | Cmd::Update | Cmd::Status | Cmd::Wait | Cmd::Dev(_)
    );
    let r = match cmd {
        Cmd::Setup(a) => setup::setup(a).await,
        Cmd::Uninstall(a) => setup::uninstall(a).await,
        Cmd::Login(a) => login::login(a).await,
        Cmd::Logout => account::logout().await,
        Cmd::Whoami => account::whoami().await,
        Cmd::Rename { name } => account::rename(name).await,
        Cmd::Device(c) => account::device(c).await,
        Cmd::Contact(c) => contact::run(c).await,
        Cmd::Group(c) => group::run(c).await,
        Cmd::Send { to, body, files } => messages::send(to, body.join(" "), files).await,
        Cmd::Get { message_id } => messages::get(message_id).await,
        Cmd::Inbox { message_id, all } => messages::inbox(message_id, all).await,
        Cmd::Status => status::status().await,
        Cmd::Update => crate::update::update().await,
        Cmd::Wait => wait::wait().await,
        Cmd::Dev(c) => dev::run(c).await,
        Cmd::Daemon | Cmd::Mcp | Cmd::Hook { .. } => unreachable!("handled in main"),
    };
    if hint {
        crate::update::hint();
    }
    r
}

/// Print a problem the way every command does, and return `Reported`.
pub fn report(p: &Problem) -> anyhow::Error {
    ui::error_line(&ui::heading(&p.title));
    eprintln!("  {}", ui::s(ui::text(), &p.detail));
    if let Some(cands) = &p.candidates {
        eprintln!();
        eprintln!("  {}", ui::dim("Did you mean:"));
        for c in cands {
            let state = match c.online {
                Some(true) => format!(" {}", ui::dot(true)),
                Some(false) => format!(" {}", ui::dot(false)),
                None => String::new(),
            };
            eprintln!("    {}{}  {}", ui::heading(&c.to), state, ui::dim(&c.label));
        }
    }
    if let Some(s) = p.retry_after_s {
        eprintln!("  {}", ui::dim(format!("Try again in {s}s.")));
    }
    if let Some(id) = p.request_id() {
        eprintln!(
            "  {}",
            ui::dim(format!("ref {}", &id[id.len().saturating_sub(8)..]))
        );
    }
    Reported.into()
}

/// One REST call through the daemon, acting as the session this terminal
/// runs in (if any). Errors are printed.
pub async fn api(method: &str, path: &str, body: Option<Value>) -> anyhow::Result<Value> {
    let actor = env_actor().map_err(|p| report(&p))?;
    let local = Local::new();
    match local.call(method, path, body, actor).await {
        Ok((v, _)) => Ok(v),
        Err(p) => Err(report(&p)),
    }
}

pub async fn api_as<T: DeserializeOwned>(
    method: &str,
    path: &str,
    body: Option<Value>,
) -> anyhow::Result<T> {
    let v = api(method, path, body).await?;
    serde_json::from_value(v).map_err(|e| {
        anyhow::anyhow!("unexpected reply from the server ({e}); try `imbo setup` to update")
    })
}

/// Pad to a display width (CJK counts as two columns).
pub fn pad(s: &str, width: usize) -> String {
    let w = console::measure_text_width(s);
    if w >= width {
        s.to_string()
    } else {
        format!("{s}{}", " ".repeat(width - w))
    }
}

pub fn tool_name(tool: &str) -> &str {
    match crate::adapters::get(tool) {
        Some(a) => a.name(),
        None if tool == imbo_proto::TOOL_TERMINAL => "Terminal",
        None => tool,
    }
}

/// `#5520183` / `5520183` / a group name → group ID.
pub async fn group_id(g: &str) -> anyhow::Result<i32> {
    let t = g.trim().trim_start_matches('#');
    if t.len() == 7 && t.chars().all(|c| c.is_ascii_digit()) {
        return Ok(t.parse()?);
    }
    let r: imbo_proto::api::GroupsResponse = api_as("GET", "/v1/groups", None).await?;
    let hits: Vec<_> = r
        .groups
        .iter()
        .filter(|x| x.name.eq_ignore_ascii_case(g.trim()))
        .collect();
    match hits.as_slice() {
        [one] => Ok(one.id),
        [] => {
            ui::error_line(&format!("You're not in a group called {}.", ui::heading(g)));
            eprintln!("  {}", ui::dim("See your groups with `imbo group list`."));
            Err(Reported.into())
        }
        _ => {
            ui::error_line(&format!(
                "More than one group is called {}; use its #ID.",
                ui::heading(g)
            ));
            Err(Reported.into())
        }
    }
}
