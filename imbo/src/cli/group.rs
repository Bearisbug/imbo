//! `imbo group …` (FLOW-GROUP).

use clap::Subcommand;
use imbo_proto::api::{
    AcceptResponse, AgentRef, AgentsResponse, GroupResponse, GroupView, GroupsResponse,
    MySessionsResponse, RequestResponse, RequestsResponse,
};
use imbo_proto::text::validate_group_name;
use serde_json::json;
use uuid::Uuid;

use super::{Reported, api, api_as, group_id, pad, tool_name};
use crate::ui;

#[derive(Subcommand)]
pub enum GroupCmd {
    /// Groups you're in.
    List,
    /// Create a group.
    Create {
        name: String,
        /// Your sessions that get its messages (short IDs, comma-separated).
        #[arg(long, value_delimiter = ',')]
        sessions: Option<Vec<String>>,
    },
    /// Invite a contact to a group.
    Invite { group: String, account_id: i32 },
    /// Accept a group invite (request ID, group name or #ID).
    Join {
        invite: String,
        #[arg(long, value_delimiter = ',')]
        sessions: Option<Vec<String>>,
    },
    /// Choose which of your sessions get a group's messages.
    Sessions {
        group: String,
        #[arg(long, value_delimiter = ',')]
        sessions: Option<Vec<String>>,
    },
    /// Leave a group.
    Leave {
        group: String,
        #[arg(long, short)]
        yes: bool,
    },
}

pub async fn run(cmd: GroupCmd) -> anyhow::Result<()> {
    match cmd {
        GroupCmd::List => list().await,
        GroupCmd::Create { name, sessions } => create(name, sessions).await,
        GroupCmd::Invite { group, account_id } => {
            let gid = group_id(&group).await?;
            let r: RequestResponse = api_as(
                "POST",
                &format!("/v1/groups/{gid}/invites"),
                Some(json!({ "account_id": account_id })),
            )
            .await?;
            ui::ok_line(&format!(
                "Invited {} {}  {}",
                ui::heading(&r.request.to.name),
                ui::dim(format!("({account_id})")),
                ui::dim("they join once they accept")
            ));
            Ok(())
        }
        GroupCmd::Join { invite, sessions } => join(invite, sessions).await,
        GroupCmd::Sessions { group, sessions } => set_sessions(group, sessions).await,
        GroupCmd::Leave { group, yes } => {
            let gid = group_id(&group).await?;
            if !yes && ui::is_interactive() {
                let go: bool = cliclack::confirm(format!("Leave {group}? Its messages to your sessions that haven't arrived are canceled."))
                    .initial_value(false)
                    .interact()?;
                if !go {
                    return Ok(());
                }
            }
            api("DELETE", &format!("/v1/groups/{gid}/members/me"), None).await?;
            ui::ok_line(&format!("Left {}", ui::heading(&group)));
            Ok(())
        }
    }
}

fn print_group(g: &GroupView) {
    println!(
        "{}  {}",
        ui::heading(&g.name),
        ui::dim(format!("#{}", g.id))
    );
    let members: Vec<&str> = g.members.iter().map(|m| m.name.as_str()).collect();
    println!(
        "  {} {}",
        ui::dim("members "),
        ui::s(ui::text(), members.join(", "))
    );
    if g.my_sessions.is_empty() {
        println!(
            "  {} {}",
            ui::dim("receiving"),
            ui::s(ui::warn(), "none of your sessions")
        );
    } else {
        let mine: Vec<String> = g
            .my_sessions
            .iter()
            .map(|a| format!("{} {}", a.title, ui::dim(&a.short_id)))
            .collect();
        println!("  {} {}", ui::dim("receiving"), mine.join(", "));
    }
}

async fn list() -> anyhow::Result<()> {
    let r: GroupsResponse = api_as("GET", "/v1/groups", None).await?;
    if r.groups.is_empty() {
        println!(
            "{}",
            ui::dim("You're not in any group. Create one with `imbo group create <name>`.")
        );
    }
    for g in &r.groups {
        print_group(g);
    }
    Ok(())
}

async fn my_agents() -> anyhow::Result<Vec<imbo_proto::api::Agent>> {
    let r: AgentsResponse = api_as("GET", "/v1/agents", None).await?;
    Ok(r.agents)
}

/// Session short IDs from a flag → agent IDs.
fn resolve(agents: &[imbo_proto::api::Agent], wanted: &[String]) -> anyhow::Result<Vec<Uuid>> {
    let mut out = vec![];
    for w in wanted.iter().map(|w| w.trim()).filter(|w| !w.is_empty()) {
        match agents
            .iter()
            .find(|a| a.short_id == w || a.session_id == w || a.title == w)
        {
            Some(a) => out.push(a.id),
            None => {
                ui::error_line(&format!("You have no session {w}. See `imbo status`."));
                return Err(Reported.into());
            }
        }
    }
    Ok(out)
}

/// Ask which sessions get a group's messages. `None` when there's nothing to ask.
fn choose(
    agents: &[imbo_proto::api::Agent],
    group: &str,
    current: &[AgentRef],
) -> anyhow::Result<Option<Vec<Uuid>>> {
    let shown: Vec<&imbo_proto::api::Agent> = agents
        .iter()
        .filter(|a| a.online || current.iter().any(|c| c.id == a.id))
        .collect();
    if shown.is_empty() || !ui::is_interactive() {
        return Ok(None);
    }
    let items: Vec<(Uuid, String, String)> = shown
        .iter()
        .map(|a| {
            let state = if a.online { "online" } else { "offline" };
            (
                a.id,
                pad(&a.title, 24),
                format!("{} · {} · {state}", tool_name(&a.tool), a.short_id),
            )
        })
        .collect();
    let picked: Vec<Uuid> =
        cliclack::multiselect(format!("Which of your sessions get {group}'s messages?"))
            .items(&items)
            .initial_values(current.iter().map(|c| c.id).collect())
            .required(false)
            .interact()?;
    Ok(Some(picked))
}

async fn create(name: String, sessions: Option<Vec<String>>) -> anyhow::Result<()> {
    let name = match validate_group_name(&name) {
        Ok(n) => n,
        Err(e) => {
            ui::error_line(e);
            return Err(Reported.into());
        }
    };
    let agents = my_agents().await?;
    let chosen = match sessions {
        Some(s) => Some(resolve(&agents, &s)?),
        None if matches!(crate::local::env_actor(), Ok(None)) => choose(&agents, &name, &[])?,
        None => None,
    };
    let mut body = json!({ "name": name });
    if let Some(c) = chosen {
        body["sessions"] = json!(c);
    }
    let r: GroupResponse = api_as("POST", "/v1/groups", Some(body)).await?;
    ui::ok_line(&format!(
        "Created {} {}",
        ui::heading(&r.group.name),
        ui::dim(format!("#{}", r.group.id))
    ));
    print_group(&r.group);
    eprintln!(
        "  {}",
        ui::dim(format!(
            "Invite contacts: imbo group invite {} <account ID>",
            r.group.name
        ))
    );
    Ok(())
}

pub async fn join(invite: String, sessions: Option<Vec<String>>) -> anyhow::Result<()> {
    let r: RequestsResponse =
        api_as("GET", "/v1/requests?direction=in&status=pending", None).await?;
    let t = invite.trim().trim_start_matches('#');
    let hit = r.requests.iter().filter(|q| q.group.is_some()).find(|q| {
        let g = q.group.as_ref().unwrap();
        q.id.to_string() == t || g.id.to_string() == t || g.name.eq_ignore_ascii_case(invite.trim())
    });
    let Some(q) = hit else {
        ui::error_line(&format!(
            "No pending group invite matches {invite}. See `imbo contact requests`."
        ));
        return Err(Reported.into());
    };
    let gname = q.group.as_ref().map(|g| g.name.clone()).unwrap_or_default();
    let agents = my_agents().await?;
    let chosen = match sessions {
        Some(s) => Some(resolve(&agents, &s)?),
        None => choose(&agents, &gname, &[])?,
    };
    let body = match chosen {
        Some(c) => json!({ "sessions": c }),
        None => json!({}),
    };
    let a: AcceptResponse =
        api_as("POST", &format!("/v1/requests/{}/accept", q.id), Some(body)).await?;
    let g = a.group.map(|g| (g.name, g.id)).unwrap_or((gname, 0));
    ui::ok_line(&format!(
        "Joined {} {}",
        ui::heading(&g.0),
        ui::dim(format!("#{}", g.1))
    ));
    let mine = a.my_sessions.unwrap_or_default();
    if mine.is_empty() {
        eprintln!(
            "  {}",
            ui::s(ui::warn(), "None of your sessions get its messages yet.")
        );
        eprintln!(
            "  {}",
            ui::dim(format!("Choose some: imbo group sessions {}", g.0))
        );
    } else {
        let names: Vec<String> = mine
            .iter()
            .map(|a| format!("{} {}", a.title, ui::dim(&a.short_id)))
            .collect();
        eprintln!("  {} {}", ui::dim("receiving"), names.join(", "));
    }
    Ok(())
}

async fn set_sessions(group: String, sessions: Option<Vec<String>>) -> anyhow::Result<()> {
    let gid = group_id(&group).await?;
    let groups: GroupsResponse = api_as("GET", "/v1/groups", None).await?;
    let current = groups
        .groups
        .iter()
        .find(|g| g.id == gid)
        .map(|g| g.my_sessions.clone())
        .unwrap_or_default();
    let agents = my_agents().await?;
    let chosen = match sessions {
        Some(s) => resolve(&agents, &s)?,
        None => match choose(&agents, &group, &current)? {
            Some(c) => c,
            None => {
                ui::error_line(
                    "Nothing to choose from: no sessions online. Pass --sessions <short IDs>.",
                );
                return Err(Reported.into());
            }
        },
    };
    let r: MySessionsResponse = api_as(
        "PUT",
        &format!("/v1/groups/{gid}/sessions"),
        Some(json!({ "sessions": chosen })),
    )
    .await?;
    if r.my_sessions.is_empty() {
        ui::ok_line(&format!(
            "None of your sessions get {}'s messages now",
            ui::heading(&group)
        ));
    } else {
        let names: Vec<String> = r
            .my_sessions
            .iter()
            .map(|a| format!("{} {}", a.title, ui::dim(&a.short_id)))
            .collect();
        ui::ok_line(&format!(
            "{} messages go to: {}",
            ui::heading(&group),
            names.join(", ")
        ));
    }
    Ok(())
}
