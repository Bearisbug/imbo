//! `imbo send` and `imbo inbox`.

use imbo_proto::api::{
    InboxItem, InboxResponse, MessageResponse, MessageView, PullResponse, SendResponse,
};
use imbo_proto::{ErrorCode, MAX_BODY_CHARS, TOOL_TERMINAL};
use serde_json::json;

use super::{Reported, api_as, report, tool_name};
use crate::local::{Local, env_actor};
use crate::{render, ui};

pub async fn send(to: String, body: String) -> anyhow::Result<()> {
    let n = body.chars().count();
    if body.trim().is_empty() || n > MAX_BODY_CHARS {
        ui::error_line(&format!(
            "The message must be 1–{MAX_BODY_CHARS} characters (this one is {n})."
        ));
        return Err(Reported.into());
    }
    let req = json!({ "to": to, "body": body, "client_msg_id": uuid::Uuid::now_v7().to_string() });
    let r: SendResponse = api_as("POST", "/v1/messages", Some(req)).await?;
    match &r.conversation.group {
        Some(g) => ui::ok_line(&format!(
            "Sent #{} to {} {}  {}",
            r.message_id,
            ui::heading(&g.name),
            ui::dim(format!("#{}", g.id)),
            ui::dim(sessions(r.recipients))
        )),
        None => {
            let peer = r
                .conversation
                .peer
                .as_ref()
                .map(|p| p.name.clone())
                .unwrap_or(to);
            if r.terminal {
                ui::ok_line(&format!(
                    "Sent #{} to {}'s terminal inbox  {}",
                    r.message_id,
                    ui::heading(peer),
                    ui::dim("they read it with `imbo inbox`")
                ));
                return Ok(());
            }
            ui::ok_line(&format!(
                "Sent #{} to {}  {}",
                r.message_id,
                ui::heading(peer),
                ui::dim(format!(
                    "{} · arrives when their session is online",
                    sessions(r.recipients)
                ))
            ));
        }
    }
    Ok(())
}

fn sessions(n: u32) -> String {
    if n == 1 {
        "1 session".into()
    } else {
        format!("{n} sessions")
    }
}

fn print_message(m: &MessageView, indent: &str, full: bool, state: &str) {
    println!(
        "{indent}{}  {}  {}  {state}",
        ui::s(ui::text(), format!("#{}", m.id)),
        ui::heading(render::sender(m)),
        ui::dim(ui::ago(m.created_at))
    );
    let body = if full {
        m.body.clone()
    } else {
        let (t, cut) = imbo_proto::text::truncate_chars(&m.body, 300);
        if cut {
            format!("{t}… (imbo inbox {})", m.id)
        } else {
            t
        }
    };
    for line in body.lines() {
        println!("{indent}  {line}");
    }
    if full {
        println!("{indent}{}", ui::dim(format!("reply to: {}", m.reply_to)));
    }
}

pub async fn inbox(id: Option<i64>, all: bool) -> anyhow::Result<()> {
    if let Some(id) = id {
        let r: MessageResponse = api_as("GET", &format!("/v1/messages/{id}"), None).await?;
        print_message(&r.message, "", true, "");
        return Ok(());
    }
    // Outside a session, new terminal-inbox messages are shown once and
    // count as read (ADR-010); inside one, only that session is listed.
    let mut fresh: Vec<MessageView> = vec![];
    if env_actor().is_none() {
        match Local::new()
            .call("POST", "/v1/inbox/pull", None, None)
            .await
        {
            Ok((v, _)) => fresh = serde_json::from_value::<PullResponse>(v)?.messages,
            // Servers before v0.1.1 have no terminal inbox and refuse this pull.
            Err(p) if p.is(ErrorCode::Forbidden) => {}
            Err(p) => return Err(report(&p)),
        }
    }
    if !fresh.is_empty() {
        println!(
            "{}  {}",
            ui::heading("Terminal inbox"),
            ui::dim(format!("{} new", fresh.len()))
        );
        for m in &fresh {
            print_message(m, "  ", false, &ui::s(ui::unread(), "new").to_string());
        }
    }
    let path = if all {
        "/v1/inbox?all=true"
    } else {
        "/v1/inbox"
    };
    let r: InboxResponse = api_as("GET", path, None).await?;
    // The messages just shown above are already marked read.
    let items: Vec<_> = r
        .items
        .iter()
        .filter(|it| {
            !fresh
                .iter()
                .any(|m| m.id == it.message.id && it.agent.tool == TOOL_TERMINAL)
        })
        .collect();
    if items.is_empty() {
        if fresh.is_empty() {
            println!(
                "{}",
                ui::dim(
                    "Nothing waiting. Messages show up here until they reach a session or you read them here."
                )
            );
        }
        return Ok(());
    }
    if !fresh.is_empty() {
        println!();
    }
    // One block per session (or the terminal inbox), in the order each first appears.
    let mut groups: Vec<Vec<&InboxItem>> = vec![];
    for it in items {
        match groups.iter_mut().find(|g| g[0].agent.id == it.agent.id) {
            Some(g) => g.push(it),
            None => groups.push(vec![it]),
        }
    }
    for (i, group) in groups.iter().enumerate() {
        if i > 0 {
            println!();
        }
        let agent = &group[0].agent;
        if agent.tool == TOOL_TERMINAL {
            println!("{}", ui::heading("Terminal inbox"));
        } else {
            println!(
                "{}  {}",
                ui::heading(&agent.title),
                ui::dim(format!("{} · {}", tool_name(&agent.tool), agent.short_id))
            );
        }
        for it in group {
            let state = match it.state.as_str() {
                "delivered" if agent.tool == TOOL_TERMINAL => ui::dim("read"),
                "delivered" => ui::s(ui::ok(), "delivered").to_string(),
                "canceled" => ui::dim("canceled"),
                _ => ui::s(ui::unread(), "waiting").to_string(),
            };
            print_message(&it.message, "  ", false, &state);
        }
    }
    Ok(())
}
