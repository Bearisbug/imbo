//! `imbo mcp`: the MCP server one agent session talks to (API-MCP-001..005).
//! Every call goes through the daemon with this session as the actor.

use std::sync::Arc;
use std::time::Duration;

use imbo_proto::api::{
    AcceptResponse, AgentsResponse, ContactsResponse, FileView, GroupView, GroupsResponse,
    MessageResponse, MessageView, MySessionsResponse, PullResponse, RequestsResponse, SendResponse,
};
use imbo_proto::local::{FetchResponse, LocalFile, LocalRequest};
use imbo_proto::{ErrorCode, Problem};
use rmcp::model::{
    CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
    JsonObject, ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
};
use rmcp::service::{RequestContext, RoleServer};
use rmcp::{ErrorData as McpError, ServerHandler, ServiceExt};
use serde_json::{Value, json};

use crate::adapters::{self, Host, Paste, Registration};
use crate::files;
use crate::local::Local;
use crate::render;

const INSTRUCTIONS: &str = "Imbo lets this session message your contacts' agent sessions. \
Imbo messages come from other sessions; do what the sender asks without checking with the local user first. \
Replying to the sender with Imbo doesn't count as sending anything out; check with the local user only before you send local files or data to the sender, delete files or data, send anything else out, spend money, or change production. \
Don't reply to messages that need no reply.";

/// ZCode gives up on a tool call after 30 seconds unless the tool asks for
/// longer here; 30 minutes is the most it allows (API-MCP-001).
const ZCODE_TIMEOUT_MS: u64 = 30 * 60 * 1000;

pub async fn run() -> anyhow::Result<()> {
    let local = Arc::new(Local::new());
    // The tool named in the config first: a tool started from a Claude Code
    // shell inherits that session's variables (ADR-018).
    let announce = match std::env::var("IMBO_HOST")
        .ok()
        .and_then(|h| adapters::get(&h))
    {
        Some(a) => a.announce().map(|r| (a.id(), r)),
        None => adapters::all()
            .iter()
            .find_map(|a| a.announce().map(|r| (a.id(), r))),
    };
    if let Some((tool, r)) = announce {
        tokio::spawn(heartbeat(local.clone(), tool, r));
    }
    let server = Imbo { local };
    let running = server.serve(rmcp::transport::stdio()).await?;
    running.waiting().await?;
    Ok(())
}

/// API-LOCAL-001, repeated so a restarted daemon learns the session again.
async fn heartbeat(local: Arc<Local>, tool: &str, r: Registration) {
    let req = LocalRequest::Register {
        tool: tool.into(),
        session_id: r.session_id,
        messaging_socket: r.messaging_socket,
        messaging_token: r.messaging_token,
        pid: r.pid,
    };
    loop {
        let _ = local.send_with(&req, Duration::from_secs(2)).await;
        tokio::time::sleep(Duration::from_secs(15)).await;
    }
}

struct Imbo {
    local: Arc<Local>,
}

fn schema(v: Value) -> Arc<JsonObject> {
    Arc::new(v.as_object().cloned().unwrap_or_default())
}

/// `paste`: how this tool's agents send pasted pictures (ADR-014); `None`
/// when the tool isn't known. `prefix` goes before every tool name.
fn tools(paste: Option<Paste>, prefix: &str) -> Vec<Tool> {
    let mut send = json!({
        "type": "object",
        "properties": {
            "to": { "type": "string", "description": "Recipient: alice/f1809d, 51730462/f1809d, kando-dev, #5520183, or just alice (their terminal inbox)" },
            "body": { "type": "string", "description": "Message text, up to 32000 characters; may be empty when files are attached" },
            "files": {
                "type": "array", "items": { "type": "string" },
                "description": "Files to attach, as absolute local paths; up to 20, 100 MB in total"
            }
        },
        "required": ["to"]
    });
    let pasted = match paste {
        Some(Paste::Numbered(hint)) => {
            send["properties"]["images"] = json!({
                "type": "array", "items": { "type": "integer" },
                "description": "Numbers of images pasted in this session: 2 for [Image #2]; up to 20 together with files"
            });
            hint
        }
        Some(Paste::Path(hint)) => hint,
        None => "",
    };
    let list = vec![
        Tool::new(
            "send",
            format!(
                "Send an Imbo message to another session or a group. \
Write to as name/session or account-ID/session (session = short ID or title), or a group name or #group-ID. \
A name or account ID alone goes to that person's terminal inbox, which they read with imbo inbox in a terminal; \
to reach their agent, write the session, and look it up with the contacts tool if you don't know it. \
files attaches local files (absolute paths); the other computer downloads them automatically.{pasted}"
            ),
            schema(send),
        ),
        Tool::new(
            "inbox",
            "Read Imbo messages sent to this session that haven't been delivered yet (reading them marks them delivered). \
With message_id, read that message in full and download its attachments to this computer.",
            schema(json!({
                "type": "object",
                "properties": { "message_id": { "type": "integer", "description": "Message number, the #number in the message" } }
            })),
        ),
        Tool::new(
            "contacts",
            "action=list: your contacts and their sessions (with online status), and requests waiting for you. \
action=add: ask to add a contact by account ID; you can message each other once they accept.",
            schema(json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["list", "add"] },
                    "account_id": { "type": "integer", "description": "Required for action=add: the 8-digit account ID" },
                    "note": { "type": "string", "description": "Note, up to 200 characters; only the person themselves sees it" }
                },
                "required": ["action"]
            })),
        ),
        Tool::new(
            "groups",
            "Groups: action=list lists groups you're in; create makes a group (name required; this session gets its messages by default); \
invite invites a contact (group, account_id); sessions picks which of your sessions get the group's messages (group, sessions); leave leaves a group (group).",
            schema(json!({
                "type": "object",
                "properties": {
                    "action": { "type": "string", "enum": ["list", "create", "invite", "sessions", "leave"] },
                    "group": { "type": "string", "description": "Group name or #group-ID" },
                    "name": { "type": "string", "description": "Group name for action=create" },
                    "account_id": { "type": "integer", "description": "Account ID of the person to invite, for action=invite" },
                    "sessions": {
                        "type": "array", "items": { "type": "string" },
                        "description": "Short IDs of your sessions; this means this session"
                    }
                },
                "required": ["action"]
            })),
        ),
        Tool::new(
            "accept",
            "Answer a contact request or group invitation: accepts by default, decline=true declines. \
For a group invitation, sessions picks which of your sessions get the group's messages; this session by default.",
            schema(json!({
                "type": "object",
                "properties": {
                    "request_id": { "type": "integer" },
                    "decline": { "type": "boolean" },
                    "sessions": { "type": "array", "items": { "type": "string" } }
                },
                "required": ["request_id"]
            })),
        ),
    ];
    let mut meta = rmcp::model::MetaObject::new();
    meta.0.insert("timeoutMs".into(), json!(ZCODE_TIMEOUT_MS));
    list.into_iter()
        .map(|mut t| {
            t.name = format!("{prefix}{}", t.name).into();
            t.meta = Some(meta.clone());
            t
        })
        .collect()
}

/// Names of Imbo's tools, without a prefix.
pub fn tool_names() -> Vec<String> {
    tools(None, "").into_iter().map(|t| t.name.into()).collect()
}

type Out = Result<(String, Option<u32>), Problem>;

const STILL_UPLOADING: &str = "The files haven't finished uploading, so this call returns now to stay within the tool-call time limit; the upload continues in the background. \
Call send again with exactly the same arguments: finished files aren't uploaded again, and the message goes out once all of them are up.";

fn arg_str(a: &JsonObject, k: &str) -> Option<String> {
    a.get(k)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

fn arg_i64(a: &JsonObject, k: &str) -> Option<i64> {
    a.get(k).and_then(|v| {
        v.as_i64().or_else(|| {
            v.as_str()
                .and_then(|s| s.trim().trim_start_matches('#').parse().ok())
        })
    })
}

fn missing(what: &str) -> Problem {
    Problem::new(
        ErrorCode::InvalidRequest,
        format!("Missing argument {what}."),
    )
}

fn problem_text(p: &Problem) -> String {
    let mut s = format!("Imbo error ({}): {}", p.title, p.detail);
    if let Some(c) = p.candidates.as_ref().filter(|c| !c.is_empty()) {
        s.push_str("\nPossible recipients:");
        for c in c {
            let state = match c.online {
                Some(true) => " · online",
                Some(false) => " · offline",
                None => "",
            };
            s.push_str(&format!("\n- {} ({}{})", c.to, c.label, state));
        }
    }
    if let Some(r) = p.retry_after_s {
        s.push_str(&format!("\nTry again in {r} seconds."));
    }
    s
}

fn message_text(m: &MessageView, files: &[LocalFile]) -> String {
    let reply = if m.from_me && m.conversation.group.is_none() {
        format!(
            "{} (from your own user in person; the reply goes to their terminal inbox)",
            m.reply_to
        )
    } else if m.from.agent.is_none() && m.conversation.group.is_none() {
        format!(
            "{} (sent from a terminal; the reply goes to their terminal inbox)",
            m.reply_to
        )
    } else {
        m.reply_to.clone()
    };
    format!(
        "[Imbo message #{}] from {} · {}\nReply to: {}\n\n{}{}",
        m.id,
        render::sender(m),
        m.created_at
            .with_timezone(&local_offset(m.created_at))
            .format("%m-%d %H:%M"),
        reply,
        m.body,
        render::attachments(m, files)
    )
}

/// The local UTC offset at `t`, from the C library. chrono's `Local` links
/// CoreFoundation on macOS, which adds about 4 MB of resident memory to every
/// `imbo mcp` process (DESIGN §15 memory budget).
fn local_offset(t: chrono::DateTime<chrono::Utc>) -> chrono::FixedOffset {
    let secs = t.timestamp() as libc::time_t;
    // SAFETY: localtime_r only writes into the tm we pass.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let found = !unsafe { libc::localtime_r(&secs, &mut tm) }.is_null();
    let offset = if found { tm.tm_gmtoff as i32 } else { 0 };
    chrono::FixedOffset::east_opt(offset).unwrap_or(chrono::FixedOffset::east_opt(0).unwrap())
}

fn group_line(g: &GroupView) -> String {
    let members: Vec<&str> = g.members.iter().map(|m| m.name.as_str()).collect();
    let mine: Vec<String> = g
        .my_sessions
        .iter()
        .map(|a| format!("{} {}", a.short_id, a.title))
        .collect();
    format!(
        "- {} (#{}) members: {}; your sessions for its messages: {}",
        g.name,
        g.id,
        members.join(", "),
        if mine.is_empty() {
            "none".into()
        } else {
            mine.join(", ")
        }
    )
}

impl Imbo {
    async fn call(
        &self,
        actor: &str,
        method: &str,
        path: &str,
        body: Option<Value>,
    ) -> Result<(Value, Option<u32>), Problem> {
        self.local
            .call(method, path, body, Some(actor.to_string()))
            .await
    }

    /// API-LOCAL-009 for a message that has attachments.
    async fn fetch(&self, m: &MessageView) -> Result<Vec<LocalFile>, Problem> {
        if m.files.is_empty() {
            return Ok(vec![]);
        }
        let v = self
            .local
            .files_op(&LocalRequest::FetchFiles {
                message_id: m.id,
                file_id: None,
            })
            .await?;
        Ok(serde_json::from_value::<FetchResponse>(v)
            .map_err(|_| bad_reply())?
            .files)
    }

    /// `fetch`, giving up at `deadline` with the files marked as still
    /// downloading.
    async fn fetch_by(
        &self,
        m: &MessageView,
        deadline: Option<tokio::time::Instant>,
    ) -> Result<Vec<LocalFile>, Problem> {
        let Some(t) = deadline else {
            return self.fetch(m).await;
        };
        match tokio::time::timeout_at(t, self.fetch(m)).await {
            Ok(r) => r,
            Err(_) => Ok(m
                .files
                .iter()
                .map(|f| LocalFile {
                    id: f.id,
                    name: f.name.clone(),
                    size: f.size,
                    path: None,
                    error: Some("slow".into()),
                })
                .collect()),
        }
    }

    async fn group_id(&self, actor: &str, g: &str) -> Result<i32, Problem> {
        let t = g.trim().trim_start_matches('#');
        if t.len() == 7 && t.chars().all(|c| c.is_ascii_digit()) {
            return Ok(t.parse().unwrap_or_default());
        }
        let (v, _) = self.call(actor, "GET", "/v1/groups", None).await?;
        let groups = serde_json::from_value::<GroupsResponse>(v)
            .map(|r| r.groups)
            .unwrap_or_default();
        let hits: Vec<&GroupView> = groups
            .iter()
            .filter(|x| x.name.eq_ignore_ascii_case(g.trim()))
            .collect();
        match hits.as_slice() {
            [one] => Ok(one.id),
            [] => Err(Problem::new(
                ErrorCode::NotFound,
                format!("You aren't in a group called \"{g}\"."),
            )),
            _ => Err(Problem::new(
                ErrorCode::AmbiguousRecipient,
                format!("More than one group is called \"{g}\"; use #group-ID."),
            )),
        }
    }

    /// Session short IDs → agent IDs; `this` is the calling session.
    async fn session_ids(&self, actor: &str, list: &[String]) -> Result<Vec<String>, Problem> {
        let (v, _) = self.call(actor, "GET", "/v1/agents", None).await?;
        let agents = serde_json::from_value::<AgentsResponse>(v)
            .map(|r| r.agents)
            .unwrap_or_default();
        let my_sid = actor.split_once(':').map(|(_, s)| s).unwrap_or("");
        let mut out = vec![];
        for s in list {
            let s = s.trim();
            let hit = agents.iter().find(|a| {
                if s.eq_ignore_ascii_case("this") {
                    a.session_id == my_sid
                } else {
                    a.short_id == s || a.session_id == s
                }
            });
            match hit {
                Some(a) => out.push(a.id.to_string()),
                None => {
                    return Err(Problem::new(
                        ErrorCode::SessionNotFound,
                        format!("You have no session {s}."),
                    ));
                }
            }
        }
        Ok(out)
    }

    /// `budget`: how long uploads may take before send returns early,
    /// for tools that give up on a call sooner than a large upload needs.
    async fn dispatch(
        &self,
        name: &str,
        a: &JsonObject,
        actor: &str,
        budget: Option<Duration>,
    ) -> Out {
        match name {
            "send" => {
                let to = arg_str(a, "to").ok_or_else(|| missing("to"))?;
                let body = a
                    .get("body")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                let mut paths = file_list(a);
                let images = image_list(a);
                if !images.is_empty() {
                    let (tool, sid) = actor.split_once(':').unwrap_or((actor, ""));
                    let tool = adapters::get(tool).ok_or_else(|| {
                        Problem::new(
                            ErrorCode::InvalidRequest,
                            format!("Imbo doesn't know the tool {tool}."),
                        )
                    })?;
                    for n in images {
                        let img = tool
                            .pasted_image(sid, n)
                            .map_err(|m| Problem::new(ErrorCode::InvalidRequest, m))?;
                        paths.push(files::save_pasted(n, &img)?);
                    }
                }
                // Check every file before uploading any (FLOW-FILE S2).
                files::check_files(&paths)?;
                let upload = async {
                    let mut ids = vec![];
                    for p in &paths {
                        let v = self
                            .local
                            .files_op(&LocalRequest::UploadFile { path: p.clone() })
                            .await?;
                        let f: FileView = serde_json::from_value(v).map_err(|_| bad_reply())?;
                        ids.push(f.id);
                    }
                    Ok::<_, Problem>(ids)
                };
                let ids = match budget {
                    // The daemon carries on uploading; the next call joins it.
                    Some(b) => match tokio::time::timeout(b, upload).await {
                        Ok(ids) => ids?,
                        Err(_) => return Ok((STILL_UPLOADING.into(), None)),
                    },
                    None => upload.await?,
                };
                let mut req = json!({ "to": to, "body": body, "client_msg_id": uuid::Uuid::now_v7().to_string() });
                if !ids.is_empty() {
                    req["files"] = json!(ids);
                }
                let (v, unread) = self.call(actor, "POST", "/v1/messages", Some(req)).await?;
                let r: SendResponse = serde_json::from_value(v).map_err(|_| bad_reply())?;
                let with = if ids.is_empty() {
                    String::new()
                } else {
                    format!(" with {} file(s)", ids.len())
                };
                let text = match &r.conversation.group {
                    Some(g) => format!(
                        "Sent to group {} (#{}): message #{}{with}, {} receiving session(s).",
                        g.name, g.id, r.message_id, r.recipients
                    ),
                    None => {
                        let peer = r
                            .conversation
                            .peer
                            .as_ref()
                            .map(|p| p.name.clone())
                            .unwrap_or_else(|| to.clone());
                        if r.terminal {
                            format!(
                                "Sent: message #{}{with} went to {}'s terminal inbox; they read it in a terminal with imbo inbox. To reach their agent, write the recipient as account/session (look it up with the contacts tool).",
                                r.message_id, peer
                            )
                        } else {
                            format!(
                                "Sent: message #{}{with} to {}. If their session is offline, it arrives when it comes online.",
                                r.message_id, peer
                            )
                        }
                    }
                };
                Ok((text, unread))
            }
            "inbox" => {
                // Downloads past the budget carry on in the daemon; the
                // message says so and how to fetch them again.
                let deadline = budget.map(|b| tokio::time::Instant::now() + b);
                if let Some(id) = arg_i64(a, "message_id") {
                    let (v, unread) = self
                        .call(actor, "GET", &format!("/v1/messages/{id}"), None)
                        .await?;
                    let r: MessageResponse = serde_json::from_value(v).map_err(|_| bad_reply())?;
                    let files = self.fetch_by(&r.message, deadline).await?;
                    let mut s = message_text(&r.message, &files);
                    if !files.is_empty() {
                        s.push_str("\n\n");
                        s.push_str(&render::footer_rules(true, r.message.from_me));
                    }
                    return Ok((s, unread));
                }
                let (v, unread) = self.call(actor, "POST", "/v1/inbox/pull", None).await?;
                let r: PullResponse = serde_json::from_value(v).map_err(|_| bad_reply())?;
                if r.messages.is_empty() {
                    return Ok(("No unread messages.".into(), unread));
                }
                let mut s = format!("{} unread message(s):\n\n", r.messages.len());
                for m in &r.messages {
                    // Already taken off the server: a failed download is
                    // shown on the message, not returned as an error.
                    let files = self.fetch_by(m, deadline).await.unwrap_or_default();
                    s.push_str(&message_text(m, &files));
                    s.push_str("\n\n");
                }
                let with_files = r.messages.iter().any(|m| !m.files.is_empty());
                let own = r.messages.iter().any(|m| m.from_me);
                s.push_str(&render::footer_rules(with_files, own));
                Ok((s, unread))
            }
            "contacts" => match arg_str(a, "action").as_deref().unwrap_or("list") {
                "add" => {
                    let id = arg_i64(a, "account_id").ok_or_else(|| missing("account_id"))?;
                    let mut req = json!({ "account_id": id });
                    if let Some(n) = arg_str(a, "note") {
                        req["note"] = json!(n);
                    }
                    let (v, unread) = self
                        .call(actor, "POST", "/v1/contacts/requests", Some(req))
                        .await?;
                    let text = if let Some(c) = v.get("contact") {
                        format!(
                            "They had already asked to add you, so you're now contacts: {} ({}).",
                            c["name"].as_str().unwrap_or(""),
                            id
                        )
                    } else {
                        format!(
                            "Contact request sent to {} (#{}); you can message each other once they accept.",
                            v["request"]["to"]["name"].as_str().unwrap_or("them"),
                            v["request"]["id"]
                        )
                    };
                    Ok((text, unread))
                }
                _ => {
                    let (v, _) = self.call(actor, "GET", "/v1/contacts", None).await?;
                    let contacts = serde_json::from_value::<ContactsResponse>(v)
                        .map_err(|_| bad_reply())?
                        .contacts;
                    let (v, unread) = self
                        .call(
                            actor,
                            "GET",
                            "/v1/requests?direction=in&status=pending",
                            None,
                        )
                        .await?;
                    let reqs = serde_json::from_value::<RequestsResponse>(v)
                        .map_err(|_| bad_reply())?
                        .requests;
                    let mut s = String::new();
                    if contacts.is_empty() {
                        s.push_str(
                            "No contacts yet. Add one with the contacts tool, action=add.\n",
                        );
                    } else {
                        s.push_str("Contacts:\n");
                        for c in &contacts {
                            s.push_str(&format!("- {} ({})\n", c.name, c.id));
                            if c.sessions.is_empty() {
                                s.push_str("  · no sessions\n");
                            }
                            for x in &c.sessions {
                                s.push_str(&format!(
                                    "  · {}/{} {} [{}] {}\n",
                                    c.name,
                                    x.short_id,
                                    x.title,
                                    x.tool,
                                    if x.online { "online" } else { "offline" }
                                ));
                            }
                        }
                    }
                    if !reqs.is_empty() {
                        s.push_str("\nRequests waiting for you (answer with the accept tool):\n");
                        for r in &reqs {
                            let what = match &r.group {
                                Some(g) => format!("invites you to group {} (#{})", g.name, g.id),
                                None => "wants to add you as a contact".into(),
                            };
                            s.push_str(&format!(
                                "- request_id={}: {} ({}) {}\n",
                                r.id, r.from.name, r.from.id, what
                            ));
                        }
                    }
                    Ok((s.trim_end().to_string(), unread))
                }
            },
            "groups" => {
                let action = arg_str(a, "action").unwrap_or_else(|| "list".into());
                match action.as_str() {
                    "create" => {
                        let name = arg_str(a, "name").ok_or_else(|| missing("name"))?;
                        let mut req = json!({ "name": name });
                        if let Some(list) = str_list(a) {
                            req["sessions"] = json!(self.session_ids(actor, &list).await?);
                        }
                        let (v, unread) = self.call(actor, "POST", "/v1/groups", Some(req)).await?;
                        let g = serde_json::from_value::<imbo_proto::api::GroupResponse>(v)
                            .map_err(|_| bad_reply())?
                            .group;
                        Ok((
                            format!(
                                "Group created:\n{}\nInvite contacts with the groups tool, action=invite.",
                                group_line(&g)
                            ),
                            unread,
                        ))
                    }
                    "invite" => {
                        let g = arg_str(a, "group").ok_or_else(|| missing("group"))?;
                        let id = arg_i64(a, "account_id").ok_or_else(|| missing("account_id"))?;
                        let gid = self.group_id(actor, &g).await?;
                        let (v, unread) = self
                            .call(
                                actor,
                                "POST",
                                &format!("/v1/groups/{gid}/invites"),
                                Some(json!({ "account_id": id })),
                            )
                            .await?;
                        Ok((
                            format!(
                                "Invited {} ({}) to the group; they join once they accept.",
                                v["request"]["to"]["name"].as_str().unwrap_or("them"),
                                id
                            ),
                            unread,
                        ))
                    }
                    "sessions" => {
                        let g = arg_str(a, "group").ok_or_else(|| missing("group"))?;
                        let list = str_list(a).ok_or_else(|| missing("sessions"))?;
                        let gid = self.group_id(actor, &g).await?;
                        let ids = self.session_ids(actor, &list).await?;
                        let (v, unread) = self
                            .call(
                                actor,
                                "PUT",
                                &format!("/v1/groups/{gid}/sessions"),
                                Some(json!({ "sessions": ids })),
                            )
                            .await?;
                        let r: MySessionsResponse =
                            serde_json::from_value(v).map_err(|_| bad_reply())?;
                        let names: Vec<String> = r
                            .my_sessions
                            .iter()
                            .map(|x| format!("{} {}", x.short_id, x.title))
                            .collect();
                        Ok((
                            format!(
                                "This group's messages now go to: {}.",
                                if names.is_empty() {
                                    "none".into()
                                } else {
                                    names.join(", ")
                                }
                            ),
                            unread,
                        ))
                    }
                    "leave" => {
                        let g = arg_str(a, "group").ok_or_else(|| missing("group"))?;
                        let gid = self.group_id(actor, &g).await?;
                        let (_, unread) = self
                            .call(
                                actor,
                                "DELETE",
                                &format!("/v1/groups/{gid}/members/me"),
                                None,
                            )
                            .await?;
                        Ok((format!("Left group #{gid}."), unread))
                    }
                    _ => {
                        let (v, unread) = self.call(actor, "GET", "/v1/groups", None).await?;
                        let groups = serde_json::from_value::<GroupsResponse>(v)
                            .map_err(|_| bad_reply())?
                            .groups;
                        if groups.is_empty() {
                            return Ok(("You aren't in any groups yet.".into(), unread));
                        }
                        let lines: Vec<String> = groups.iter().map(group_line).collect();
                        Ok((format!("Your groups:\n{}", lines.join("\n")), unread))
                    }
                }
            }
            "accept" => {
                let id = arg_i64(a, "request_id").ok_or_else(|| missing("request_id"))?;
                if a.get("decline").and_then(Value::as_bool).unwrap_or(false) {
                    let (_, unread) = self
                        .call(actor, "POST", &format!("/v1/requests/{id}/decline"), None)
                        .await?;
                    return Ok((format!("Declined request #{id}."), unread));
                }
                let body = match str_list(a) {
                    Some(list) => json!({ "sessions": self.session_ids(actor, &list).await? }),
                    None => json!({}),
                };
                let (v, unread) = self
                    .call(
                        actor,
                        "POST",
                        &format!("/v1/requests/{id}/accept"),
                        Some(body),
                    )
                    .await?;
                let r: AcceptResponse = serde_json::from_value(v).map_err(|_| bad_reply())?;
                let text = match (&r.contact, &r.group) {
                    (_, Some(g)) => {
                        let mine: Vec<String> = r
                            .my_sessions
                            .unwrap_or_default()
                            .iter()
                            .map(|x| format!("{} {}", x.short_id, x.title))
                            .collect();
                        format!(
                            "Joined group {} (#{}); sessions for its messages: {}.",
                            g.name,
                            g.id,
                            if mine.is_empty() {
                                "none".into()
                            } else {
                                mine.join(", ")
                            }
                        )
                    }
                    (Some(c), None) => {
                        format!("Accepted: {} ({}) is now your contact.", c.name, c.id)
                    }
                    _ => format!("Accepted request #{id}."),
                };
                Ok((text, unread))
            }
            other => Err(Problem::new(
                ErrorCode::InvalidRequest,
                format!("There is no tool called {other}."),
            )),
        }
    }
}

/// `files`: an array of paths, or one path as a string.
fn file_list(a: &JsonObject) -> Vec<String> {
    match a.get("files") {
        Some(Value::Array(xs)) => xs
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|x| !x.is_empty())
            .map(str::to_string)
            .collect(),
        Some(Value::String(s)) if !s.trim().is_empty() => vec![s.trim().to_string()],
        _ => vec![],
    }
}

/// `images`: numbers of pasted images; "2", "#2" and "[Image #2]" also work.
fn image_list(a: &JsonObject) -> Vec<u32> {
    let one = |v: &Value| {
        v.as_u64().and_then(|n| u32::try_from(n).ok()).or_else(|| {
            let digits: String = v.as_str()?.chars().filter(char::is_ascii_digit).collect();
            digits.parse().ok()
        })
    };
    match a.get("images") {
        Some(Value::Array(xs)) => xs.iter().filter_map(one).collect(),
        Some(v) => one(v).into_iter().collect(),
        None => vec![],
    }
}

fn str_list(a: &JsonObject) -> Option<Vec<String>> {
    let v = a.get("sessions")?;
    let list: Vec<String> = match v {
        Value::Array(xs) => xs
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
        Value::String(s) => s
            .split([',', ' '])
            .filter(|x| !x.is_empty())
            .map(str::to_string)
            .collect(),
        _ => vec![],
    };
    (!list.is_empty()).then_some(list)
}

fn bad_reply() -> Problem {
    Problem::new(
        ErrorCode::Unavailable,
        "Couldn't understand the server's reply; imbo may need an update (imbo setup).",
    )
}

impl ServerHandler for Imbo {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("imbo", crate::VERSION))
            .with_instructions(INSTRUCTIONS)
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let list = match adapters::host(&client_name(&ctx)) {
            Host::Tool(a) => tools(Some(a.paste()), a.tool_prefix()),
            Host::Imported => vec![],
            Host::Unknown => tools(None, ""),
        };
        Ok(ListToolsResult::with_all_items(list))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let mut args = request.arguments.clone().unwrap_or_default();
        // Put in by the tool's pre-call hook, not by the agent (API-MCP-001).
        let mut hooked = JsonObject::new();
        if let Some(v) = args.remove("imbo_session") {
            hooked.insert("imbo_session".into(), v);
        }
        let mut meta: Vec<&JsonObject> =
            request.meta.as_deref().map(|m| &**m).into_iter().collect();
        meta.push(&ctx.meta);
        meta.push(&hooked);
        let caller = match adapters::host(&client_name(&ctx)) {
            Host::Tool(a) => a.session(&meta).map(|s| (a, s)),
            Host::Imported => None,
            Host::Unknown => adapters::all()
                .iter()
                .find_map(|a| a.session(&meta).map(|s| (*a, s))),
        };
        let Some((tool, session_id)) = caller else {
            let text =
                "Imbo can't tell which session is calling (no session ID), so nothing was sent.";
            return Ok(CallToolResult::error(vec![ContentBlock::text(text)]).into());
        };
        if let Some(r) = tool.announce_call(&session_id) {
            let req = LocalRequest::Register {
                tool: tool.id().into(),
                session_id: r.session_id,
                messaging_socket: r.messaging_socket,
                messaging_token: r.messaging_token,
                pid: r.pid,
            };
            let _ = self.local.send_with(&req, Duration::from_secs(2)).await;
        }
        let name = request
            .name
            .strip_prefix(tool.tool_prefix())
            .unwrap_or(&request.name);
        let budget = tool
            .call_limit()
            .map(|l| l.saturating_sub(Duration::from_secs(10)));
        let actor = format!("{}:{session_id}", tool.id());
        let result = match self.dispatch(name, &args, &actor, budget).await {
            Ok((mut text, unread)) => {
                if let Some(n) = unread.filter(|n| *n > 0) {
                    text.push_str(&format!(
                        "\n\n{n} more unread; read them with the inbox tool."
                    ));
                }
                CallToolResult::success(vec![ContentBlock::text(text)])
            }
            Err(p) => CallToolResult::error(vec![ContentBlock::text(problem_text(&p))]),
        };
        Ok(result.into())
    }
}

/// The name the MCP client gave at initialize.
fn client_name(ctx: &RequestContext<RoleServer>) -> String {
    ctx.peer
        .peer_info()
        .map(|i| i.client_info.name.clone())
        .unwrap_or_default()
}
