//! `imbo contact …` (FLOW-CONTACT).

use clap::Subcommand;
use imbo_proto::MAX_NOTE_CHARS;
use imbo_proto::api::{
    AcceptResponse, AgentsResponse, ContactItem, ContactsResponse, DevicesResponse, RequestItem,
    RequestsResponse, SHARE_ALL, SHARE_CHOSEN, SHARE_NONE, ShareBody, ShareResponse,
};
use serde_json::json;

use super::{Reported, api, api_as, pad, tool_name};
use crate::ui;

#[derive(Subcommand)]
pub enum ContactCmd {
    /// Contacts and their sessions.
    List,
    /// Ask someone to be your contact, by account ID or email.
    Add {
        /// Their account ID, or the email they sign in with.
        who: String,
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
    /// Which of your sessions a contact can see.
    Share {
        /// The contact's account ID or name.
        who: String,
        /// `all`, `none`, or computer names and session short IDs; leave out
        /// to see the current setting.
        items: Vec<String>,
    },
}

pub async fn run(cmd: ContactCmd) -> anyhow::Result<()> {
    match cmd {
        ContactCmd::List => list().await,
        ContactCmd::Add { who, note } => add(who, note).await,
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
        ContactCmd::Share { who, items } => share(who, items).await,
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

async fn add(who: String, note: Option<String>) -> anyhow::Result<()> {
    if let Some(n) = &note
        && n.chars().count() > MAX_NOTE_CHARS
    {
        ui::error_line(&format!(
            "The note can be at most {MAX_NOTE_CHARS} characters."
        ));
        return Err(Reported.into());
    }
    let to = match who.trim().parse::<i32>() {
        Ok(id) => json!({ "account_id": id, "note": note }),
        Err(_) if who.contains('@') => json!({ "email": who.trim(), "note": note }),
        Err(_) => {
            ui::error_line("Give an account ID or an email.");
            return Err(Reported.into());
        }
    };
    let v = api("POST", "/v1/contacts/requests", Some(to)).await?;
    if let Some(c) = v.get("contact") {
        ui::ok_line(&format!(
            "{} had already asked to add you. You're contacts now.",
            ui::heading(c["name"].as_str().unwrap_or("They"))
        ));
    } else {
        let to = &v["request"]["to"];
        ui::ok_line(&format!(
            "Request sent to {} {}",
            ui::heading(to["name"].as_str().unwrap_or("them")),
            ui::dim(format!("({})", to["id"]))
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

/// A contact by account ID or name, matched like a message recipient.
fn find_contact<'a>(contacts: &'a [ContactItem], who: &str) -> Vec<&'a ContactItem> {
    if let Ok(id) = who.parse::<i32>()
        && who.len() == 8
    {
        return contacts.iter().filter(|c| c.id == id).collect();
    }
    let exact: Vec<_> = contacts.iter().filter(|c| c.name == who).collect();
    if !exact.is_empty() {
        return exact;
    }
    contacts
        .iter()
        .filter(|c| c.name.to_lowercase() == who.to_lowercase())
        .collect()
}

/// What one item names: a computer of mine, else a session by short ID or
/// title (FLOW-CONTACT-SHARE S2).
enum Pick {
    Device(uuid::Uuid),
    Session(uuid::Uuid),
}

fn pick(item: &str, devices: &DevicesResponse, agents: &AgentsResponse) -> Result<Pick, String> {
    let d: Vec<_> = devices
        .devices
        .iter()
        .filter(|d| d.name.to_lowercase() == item.to_lowercase())
        .collect();
    if let [one] = d.as_slice() {
        return Ok(Pick::Device(one.id));
    }
    if d.len() > 1 {
        return Err(format!("You have {} computers called {item}.", d.len()));
    }
    if let Some(a) = agents
        .agents
        .iter()
        .find(|a| a.short_id.eq_ignore_ascii_case(item))
    {
        return Ok(Pick::Session(a.id));
    }
    match agents
        .agents
        .iter()
        .filter(|a| a.title == item)
        .collect::<Vec<_>>()
        .as_slice()
    {
        [one] => Ok(Pick::Session(one.id)),
        [] => Err(format!("No computer or session called {item}.")),
        many => Err(format!(
            "{} sessions are called {item}. Use the short ID.",
            many.len()
        )),
    }
}

async fn share(who: String, items: Vec<String>) -> anyhow::Result<()> {
    let r: ContactsResponse = api_as("GET", "/v1/contacts", None).await?;
    let found = find_contact(&r.contacts, who.trim());
    let contact = match found.as_slice() {
        [one] => (*one).clone(),
        [] => {
            ui::error_line(&format!("No contact called {who}."));
            return Err(Reported.into());
        }
        _ => {
            ui::error_line(&format!(
                "More than one contact is called {who}. Use the account ID."
            ));
            return Err(Reported.into());
        }
    };
    let path = format!("/v1/contacts/{}/share", contact.id);
    let view: ShareResponse = match items.as_slice() {
        [] => api_as("GET", &path, None).await?,
        [one] if one == SHARE_ALL || one == SHARE_NONE => {
            let body = ShareBody {
                share: one.clone(),
                devices: None,
                sessions: None,
            };
            api_as("PUT", &path, Some(serde_json::to_value(body)?)).await?
        }
        _ => {
            let devices: DevicesResponse = api_as("GET", "/v1/devices", None).await?;
            let agents: AgentsResponse = api_as("GET", "/v1/agents", None).await?;
            let (mut on, mut sessions) = (vec![], vec![]);
            for item in &items {
                match pick(item, &devices, &agents) {
                    Ok(Pick::Device(id)) => on.push(id),
                    Ok(Pick::Session(id)) => sessions.push(id),
                    Err(e) => {
                        ui::error_line(&e);
                        mine(&devices, &agents);
                        return Err(Reported.into());
                    }
                }
            }
            let body = ShareBody {
                share: SHARE_CHOSEN.into(),
                devices: Some(on),
                sessions: Some(sessions),
            };
            api_as("PUT", &path, Some(serde_json::to_value(body)?)).await?
        }
    };
    if !items.is_empty() {
        ui::ok_line("Saved");
    }
    show_share(&contact.name, &view);
    Ok(())
}

/// My computers and sessions, to pick from.
fn mine(devices: &DevicesResponse, agents: &AgentsResponse) {
    eprintln!("  {}", ui::dim("Your computers:"));
    for d in &devices.devices {
        eprintln!("    {}", d.name);
    }
    eprintln!("  {}", ui::dim("Your sessions:"));
    for a in &agents.agents {
        eprintln!(
            "    {} {}  {}",
            ui::dot(a.online),
            pad(&a.short_id, 8),
            a.title
        );
    }
}

fn show_share(name: &str, v: &ShareResponse) {
    match v.share.as_str() {
        SHARE_ALL => println!("{} sees all your sessions.", ui::heading(name)),
        SHARE_NONE => println!(
            "{} sees none of your sessions and can only write to your inbox.",
            ui::heading(name)
        ),
        _ if v.devices.is_empty() && v.sessions.is_empty() => println!(
            "{} sees only the sessions you choose, and none are chosen yet.",
            ui::heading(name)
        ),
        _ => {
            println!("{} sees only these:", ui::heading(name));
            for d in &v.devices {
                println!(
                    "  {}  {}",
                    pad(&d.name, 26),
                    ui::dim("every session on this computer, new ones too")
                );
            }
            for s in &v.sessions {
                println!(
                    "  {} {}  {}  {}",
                    ui::dot(s.online),
                    pad(&s.short_id, 8),
                    pad(&s.title, 24),
                    ui::dim(tool_name(&s.tool))
                );
            }
        }
    }
    println!(
        "{}",
        ui::dim("Sessions you put in a group you share are visible there too.")
    );
}
