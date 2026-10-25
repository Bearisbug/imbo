//! `imbo send`, `imbo get` and `imbo inbox`.

use std::path::PathBuf;

use imbo_proto::api::{
    FileView, InboxItem, InboxResponse, MessageResponse, MessageView, PullResponse, SendResponse,
};
use imbo_proto::local::{FetchResponse, LocalRequest};
use imbo_proto::{ErrorCode, MAX_BODY_CHARS, TOOL_TERMINAL};
use serde_json::json;

use super::{Reported, api_as, report, tool_name};
use crate::files::{self, human_size};
use crate::local::{Local, env_actor};
use crate::{render, ui};

/// One file's progress: a spinner on a terminal, a result line otherwise.
struct Step(Option<cliclack::ProgressBar>);

impl Step {
    fn start(msg: String) -> Self {
        if !console::Term::stderr().is_term() {
            return Step(None);
        }
        let sp = cliclack::spinner();
        sp.start(msg);
        Step(Some(sp))
    }

    fn ok(self, msg: String) {
        match self.0 {
            Some(sp) => sp.stop(msg),
            None => ui::ok_line(&msg),
        }
    }

    fn fail(self, msg: String) {
        match self.0 {
            Some(sp) => sp.error(msg),
            None => ui::error_line(&msg),
        }
    }
}

/// Upload each file through the daemon (API-LOCAL-008), one step per file.
async fn upload(paths: &[String]) -> anyhow::Result<Vec<FileView>> {
    files::check_files(paths).map_err(|p| report(&p))?;
    let local = Local::new();
    let mut out = vec![];
    for p in paths {
        let size = std::fs::metadata(p).map(|m| m.len()).unwrap_or_default();
        let name = std::path::Path::new(p)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let sp = Step::start(format!("Uploading {name}  {}", ui::dim(human_size(size))));
        match local
            .files_op(&LocalRequest::UploadFile { path: p.clone() })
            .await
        {
            Ok(v) => {
                let f: FileView = serde_json::from_value(v).map_err(|e| {
                    anyhow::anyhow!(
                        "unexpected reply from the daemon ({e}); try `imbo setup` to update"
                    )
                })?;
                sp.ok(format!("Uploaded {name}  {}", ui::dim(human_size(f.size))));
                out.push(f);
            }
            Err(p) => {
                sp.fail(format!("{name} was not uploaded"));
                return Err(report(&p));
            }
        }
    }
    Ok(out)
}

pub async fn send(to: String, body: String, paths: Vec<PathBuf>) -> anyhow::Result<()> {
    let n = body.chars().count();
    if (paths.is_empty() && body.trim().is_empty()) || n > MAX_BODY_CHARS {
        ui::error_line(&format!(
            "The message must be 1–{MAX_BODY_CHARS} characters (this one is {n})."
        ));
        return Err(Reported.into());
    }
    let paths: Vec<String> = paths
        .iter()
        .map(|p| {
            std::path::absolute(p)
                .unwrap_or_else(|_| p.clone())
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    let uploaded = upload(&paths).await?;
    let mut req =
        json!({ "to": to, "body": body, "client_msg_id": uuid::Uuid::now_v7().to_string() });
    if !uploaded.is_empty() {
        req["files"] = json!(uploaded.iter().map(|f| f.id).collect::<Vec<_>>());
    }
    let r: SendResponse = api_as("POST", "/v1/messages", Some(req)).await?;
    let with = match uploaded.len() {
        0 => String::new(),
        1 => " with 1 file".into(),
        n => format!(" with {n} files"),
    };
    match &r.conversation.group {
        Some(g) => ui::ok_line(&format!(
            "Sent #{}{with} to {} {}  {}",
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
                    "Sent #{}{with} to {}'s terminal inbox  {}",
                    r.message_id,
                    ui::heading(peer),
                    ui::dim("they read it with `imbo inbox`")
                ));
                return Ok(());
            }
            ui::ok_line(&format!(
                "Sent #{}{with} to {}  {}",
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

/// `imbo get`: download a message's files one by one (API-LOCAL-009).
pub async fn get(id: i64) -> anyhow::Result<()> {
    let r: MessageResponse = api_as("GET", &format!("/v1/messages/{id}"), None).await?;
    let m = r.message;
    if m.files.is_empty() {
        println!("{}", ui::dim(format!("Message #{id} has no files.")));
        return Ok(());
    }
    let home = crate::paths::Home::resolve();
    let paths = files::local_paths(&home, id, &m.files);
    let local = Local::new();
    let mut failed = false;
    for (f, p) in m.files.iter().zip(&paths) {
        if std::fs::metadata(p).is_ok_and(|x| x.len() == f.size)
            && files::sha256_file(p).is_ok_and(|(_, sha)| sha == f.sha256)
        {
            ui::ok_line(&format!("{}  {}", p.display(), ui::dim(human_size(f.size))));
            continue;
        }
        let sp = Step::start(format!(
            "Downloading {}  {}",
            f.name,
            ui::dim(human_size(f.size))
        ));
        let req = LocalRequest::FetchFiles {
            message_id: id,
            file_id: Some(f.id),
        };
        let got = match local.files_op(&req).await {
            Ok(v) => serde_json::from_value::<FetchResponse>(v)
                .ok()
                .and_then(|r| r.files.into_iter().next()),
            Err(p) => {
                sp.fail(format!("{} was not downloaded", f.name));
                return Err(report(&p));
            }
        };
        match got {
            Some(l) if l.path.is_some() => {
                sp.ok(format!(
                    "{}  {}",
                    l.path.unwrap_or_default(),
                    ui::dim(human_size(f.size))
                ));
            }
            other => {
                failed = true;
                let why = match other.and_then(|l| l.error).as_deref() {
                    Some("expired") => "expired: the server keeps files for 7 days".to_string(),
                    Some("not-found") => "not on the server".to_string(),
                    Some("disk-full") => "not enough free disk space here".to_string(),
                    Some(e) => e.trim_start_matches("failed: ").to_string(),
                    None => "no answer from the daemon".to_string(),
                };
                sp.fail(format!("{}  {}", f.name, ui::dim(why)));
            }
        }
    }
    if failed {
        return Err(Reported.into());
    }
    Ok(())
}

/// Attachment lines: the local path once downloaded, else how to get it.
fn print_files(m: &MessageView, indent: &str) {
    let home = crate::paths::Home::resolve();
    let paths = files::local_paths(&home, m.id, &m.files);
    let mut missing = false;
    for (f, p) in m.files.iter().zip(&paths) {
        let here = std::fs::metadata(p).is_ok_and(|x| x.len() == f.size);
        let place = if here {
            p.display().to_string()
        } else {
            missing = true;
            f.name.clone()
        };
        println!(
            "{indent}  {} {place}  {}",
            ui::dim("file"),
            ui::dim(human_size(f.size))
        );
    }
    if missing {
        println!(
            "{indent}  {}",
            ui::dim(format!("download with `imbo get {}`", m.id))
        );
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
    print_files(m, indent);
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
