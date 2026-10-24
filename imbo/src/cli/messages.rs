//! `imbo send` and `imbo inbox`.

use imbo_proto::MAX_BODY_CHARS;
use imbo_proto::api::{InboxResponse, MessageResponse, MessageView, SendResponse};
use serde_json::json;

use super::{Reported, api_as, tool_name};
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
            ui::dim(format!("{} sessions", r.recipients))
        )),
        None => {
            let peer = r
                .conversation
                .peer
                .as_ref()
                .map(|p| p.name.clone())
                .unwrap_or(to);
            ui::ok_line(&format!(
                "Sent #{} to {}  {}",
                r.message_id,
                ui::heading(peer),
                ui::dim("arrives when their session is online")
            ));
        }
    }
    Ok(())
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
    let path = if all {
        "/v1/inbox?all=true"
    } else {
        "/v1/inbox"
    };
    let r: InboxResponse = api_as("GET", path, None).await?;
    if r.items.is_empty() {
        println!(
            "{}",
            ui::dim("Nothing waiting. Messages show up here until they reach a session.")
        );
        return Ok(());
    }
    let mut last: Option<uuid::Uuid> = None;
    for it in &r.items {
        if last != Some(it.agent.id) {
            if last.is_some() {
                println!();
            }
            println!(
                "{}  {}",
                ui::heading(&it.agent.title),
                ui::dim(format!(
                    "{} · {}",
                    tool_name(&it.agent.tool),
                    it.agent.short_id
                ))
            );
            last = Some(it.agent.id);
        }
        let state = match it.state.as_str() {
            "delivered" => ui::s(ui::ok(), "delivered").to_string(),
            "canceled" => ui::dim("canceled"),
            _ => ui::s(ui::unread(), "waiting").to_string(),
        };
        print_message(&it.message, "  ", false, &state);
    }
    Ok(())
}
