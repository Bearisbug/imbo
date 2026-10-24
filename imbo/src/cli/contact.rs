//! `imbo contact …` (FLOW-CONTACT).

use clap::Subcommand;
use imbo_proto::MAX_NOTE_CHARS;
use imbo_proto::api::{AcceptResponse, ContactsResponse, RequestItem, RequestsResponse};
use serde_json::json;

use super::{Reported, api, api_as, pad, tool_name};
use crate::ui;

#[derive(Subcommand)]
pub enum ContactCmd {
    /// Contacts and their sessions.
    List,
    /// Ask someone to be your contact, by account ID.
    Add {
        account_id: i32,
        /// A short note only they will see (up to 200 characters).
        #[arg(long)]
        note: Option<String>,
    },
    /// Contact requests and group invites waiting for you.
    Requests {
        /// Requests you sent instead.
        #[arg(long)]
        out: bool,
        /// Include handled ones.
        #[arg(long)]
        all: bool,
    },
    /// Accept a request (group invites ask which sessions get the group's messages).
    Accept {
        request_id: i64,
        /// For group invites: session short IDs, comma-separated.
        #[arg(long, value_delimiter = ',')]
        sessions: Option<Vec<String>>,
    },
    /// Decline a request.
    Decline { request_id: i64 },
    /// Remove a contact; messages to them that haven't arrived are canceled.
    Remove {
        account_id: i32,
        #[arg(long, short)]
        yes: bool,
    },
}

pub async fn run(cmd: ContactCmd) -> anyhow::Result<()> {
    match cmd {
        ContactCmd::List => list().await,
        ContactCmd::Add { account_id, note } => add(account_id, note).await,
        ContactCmd::Requests { out, all } => requests(out, all).await,
        ContactCmd::Accept {
            request_id,
            sessions,
        } => accept(request_id, sessions).await,
        ContactCmd::Decline { request_id } => {
            api("POST", &format!("/v1/requests/{request_id}/decline"), None).await?;
            ui::ok_line(&format!("Declined request #{request_id}"));
            Ok(())
        }
        ContactCmd::Remove { account_id, yes } => remove(account_id, yes).await,
    }
}

async fn list() -> anyhow::Result<()> {
    let r: ContactsResponse = api_as("GET", "/v1/contacts?offline=true", None).await?;
    if r.contacts.is_empty() {
        println!(
            "{}",
            ui::dim("No contacts yet. Add one with `imbo contact add <account ID>`.")
        );
        return Ok(());
    }
    for c in &r.contacts {
        println!("{}  {}", ui::heading(&c.name), ui::dim(c.id));
        if c.sessions.is_empty() {
            println!("  {}", ui::dim("no sessions"));
        }
        for s in &c.sessions {
            let when = if s.online {
                String::new()
            } else {
                s.last_online_at
                    .map(|t| format!(" · {}", ui::ago(t)))
                    .unwrap_or_default()
            };
            println!(
                "  {} {}  {}  {}",
                ui::dot(s.online),
                ui::s(ui::text(), pad(&format!("{}/{}", c.name, s.short_id), 18)),
                pad(&s.title, 24),
                ui::dim(format!("{}{when}", tool_name(&s.tool)))
            );
        }
    }
    Ok(())
}

async fn add(account_id: i32, note: Option<String>) -> anyhow::Result<()> {
    if let Some(n) = &note
        && n.chars().count() > MAX_NOTE_CHARS
    {
        ui::error_line(&format!(
            "The note can be at most {MAX_NOTE_CHARS} characters."
        ));
        return Err(Reported.into());
    }
    let v = api(
        "POST",
        "/v1/contacts/requests",
        Some(json!({ "account_id": account_id, "note": note })),
    )
    .await?;
    if let Some(c) = v.get("contact") {
        ui::ok_line(&format!(
            "{} had already asked to add you. You're contacts now.",
            ui::heading(c["name"].as_str().unwrap_or("They"))
        ));
    } else {
        let name = v["request"]["to"]["name"].as_str().unwrap_or("them");
        ui::ok_line(&format!(
            "Request sent to {} {}",
            ui::heading(name),
            ui::dim(format!("({account_id})"))
        ));
        eprintln!(
            "  {}",
            ui::dim("You can message each other once they accept.")
        );
    }
    Ok(())
}

pub fn request_line(r: &RequestItem, outgoing: bool) -> String {
    let who = if outgoing { &r.to } else { &r.from };
    let what = match (&r.group, outgoing) {
        (Some(g), false) => format!(
            "invited you to {} {}",
            ui::heading(&g.name),
            ui::dim(format!("#{}", g.id))
        ),
        (Some(g), true) => format!(
            "invited to {} {}",
            ui::heading(&g.name),
            ui::dim(format!("#{}", g.id))
        ),
        (None, false) => "wants to add you".into(),
        (None, true) => "contact request".into(),
    };
    let status = if r.status == "pending" {
        String::new()
    } else {
        format!("  {}", ui::dim(&r.status))
    };
    format!(
        "{}  {} {}  {}  {}{}",
        ui::s(ui::text(), format!("#{}", r.id)),
        ui::heading(&who.name),
        ui::dim(format!("({})", who.id)),
        what,
        ui::dim(ui::ago(r.created_at)),
        status
    )
}

async fn requests(out: bool, all: bool) -> anyhow::Result<()> {
    let path = format!(
        "/v1/requests?direction={}&status={}",
        if out { "out" } else { "in" },
        if all { "all" } else { "pending" }
    );
    let r: RequestsResponse = api_as("GET", &path, None).await?;
    if r.requests.is_empty() {
        println!(
            "{}",
            ui::dim(if out {
                "No requests sent."
            } else {
                "Nothing waiting for you."
            })
        );
        return Ok(());
    }
    for q in &r.requests {
        println!("{}", request_line(q, out));
        if let Some(n) = &q.note {
            println!("      {}", ui::s(ui::text(), format!("“{n}”")));
        }
        if !out && q.status == "pending" {
            let hint = if q.group.is_some() {
                format!("imbo group join {}", q.id)
            } else {
                format!("imbo contact accept {}", q.id)
            };
            println!("      {}", ui::dim(hint));
        }
    }
    if r.notes_hidden {
        println!(
            "{}",
            ui::dim("Notes are only shown to you in your own terminal, not to agents.")
        );
    }
    Ok(())
}

async fn accept(id: i64, sessions: Option<Vec<String>>) -> anyhow::Result<()> {
    let r: RequestsResponse =
        api_as("GET", "/v1/requests?direction=in&status=pending", None).await?;
    if r.requests.iter().any(|q| q.id == id && q.group.is_some()) {
        return super::group::join(id.to_string(), sessions).await;
    }
    let a: AcceptResponse = api_as(
        "POST",
        &format!("/v1/requests/{id}/accept"),
        Some(json!({})),
    )
    .await?;
    match a.contact {
        Some(c) => ui::ok_line(&format!(
            "{} {} is a contact now",
            ui::heading(&c.name),
            ui::dim(format!("({})", c.id))
        )),
        None => ui::ok_line(&format!("Accepted request #{id}")),
    }
    Ok(())
}

async fn remove(account_id: i32, yes: bool) -> anyhow::Result<()> {
    let r: ContactsResponse = api_as("GET", "/v1/contacts?offline=true", None).await?;
    let name = r
        .contacts
        .iter()
        .find(|c| c.id == account_id)
        .map(|c| c.name.clone());
    if !yes && ui::is_interactive() {
        let who = name.clone().unwrap_or_else(|| account_id.to_string());
        let go: bool = cliclack::confirm(format!(
            "Remove {who}? Messages between you that haven't arrived are canceled."
        ))
        .initial_value(false)
        .interact()?;
        if !go {
            return Ok(());
        }
    }
    api("DELETE", &format!("/v1/contacts/{account_id}"), None).await?;
    ui::ok_line(&format!(
        "Removed {}",
        ui::heading(name.unwrap_or_else(|| account_id.to_string()))
    ));
    Ok(())
}
