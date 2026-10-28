//! Line-delimited JSON spoken on `~/.imbo/daemon.sock` (API-LOCAL-001..011).
//! Client, MCP process and daemon are the same binary, so this is not a
//! public contract.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::problem::Problem;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum LocalRequest {
    /// API-LOCAL-001: a session's tool process or hook announces it.
    Register {
        tool: String,
        session_id: String,
        messaging_socket: Option<String>,
        messaging_token: Option<String>,
        pid: u32,
    },
    /// API-LOCAL-002: forward one REST call with the device token.
    Call {
        method: String,
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        body: Option<Value>,
        /// `<tool>:<session_id>`; absent means the person.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        actor: Option<String>,
        /// Absolute path of a JPEG or PNG the daemon sends as the request
        /// body instead of `body`, as `imbo group avatar` does (API-056).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        picture: Option<String>,
    },
    /// API-LOCAL-003: a prompt or tool hook asks for messages sent back to
    /// the server.
    HookPrompt {
        tool: String,
        session_id: String,
        /// Longest context the tool accepts, in characters.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        limit: Option<usize>,
    },
    /// API-LOCAL-008: upload one local file (absolute path); the body is
    /// the `FileView`.
    UploadFile {
        path: String,
    },
    /// API-LOCAL-009: download a message's attachments (or one of them)
    /// into `~/.imbo/files/<message_id>/`; the body is a `FetchResponse`.
    FetchFiles {
        message_id: i64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        file_id: Option<uuid::Uuid>,
    },
    /// API-LOCAL-010: a listener waits for the session's next messages. The
    /// connection stays open; the daemon answers with one `WaitReply` line
    /// and, when it carries text, expects one line back once it is printed.
    Wait {
        tool: String,
        session_id: String,
        /// The tool's main process; the listener ends when it does.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pid: Option<u32>,
    },
    /// API-LOCAL-011: is a listener waiting for this session? The body is
    /// a `ListenerStatus`.
    Listening {
        tool: String,
        session_id: String,
    },
    /// The turn was aborted: drop the session's waiting listener, which
    /// ends without output (API-LOCAL-010).
    Unlisten {
        tool: String,
        session_id: String,
    },
    /// Pin a session that has no live tool behind it (tests and demos).
    FakeSession {
        tool: String,
        session_id: String,
        title: String,
        #[serde(default)]
        remove: bool,
    },
    /// Daemon state for `imbo status`.
    Status,
    /// Re-read credentials and reconnect (after login or logout).
    Reload,
    Shutdown,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalResponse {
    pub ok: bool,
    #[serde(default)]
    pub status: u16,
    #[serde(default)]
    pub body: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unread: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub problem: Option<Problem>,
}

impl LocalResponse {
    pub fn ok(status: u16, body: Value, unread: Option<u32>) -> Self {
        Self {
            ok: true,
            status,
            body,
            unread,
            problem: None,
        }
    }

    pub fn err(p: Problem) -> Self {
        Self {
            ok: false,
            status: p.status,
            body: Value::Null,
            unread: None,
            problem: Some(p),
        }
    }
}

/// What a listener gets back from API-LOCAL-010.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct WaitReply {
    /// Messages to print; `None` when the listener should end quietly
    /// (replaced, withdrawn, or the session already has one).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Another listener is already waiting for this session.
    #[serde(default)]
    pub already: bool,
}

/// API-LOCAL-011.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ListenerStatus {
    pub listening: bool,
    /// The session's last 3 listeners each ended within 60 s without
    /// handing over anything: stop asking the agent to start another.
    #[serde(default)]
    pub stalled: bool,
}

/// `imbo status` payload.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DaemonStatus {
    pub version: String,
    pub pid: u32,
    pub server: String,
    pub logged_in: bool,
    pub connected: bool,
    #[serde(default)]
    pub last_error: Option<String>,
    pub sessions: Vec<LocalSession>,
    #[serde(default)]
    pub notices: Vec<crate::frames::Notice>,
    /// The newest released version the daemon found (REQ-046).
    #[serde(default)]
    pub latest_version: Option<String>,
    /// The server's minimum client version, when this one is older (REQ-048).
    #[serde(default)]
    pub min_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalSession {
    pub tool: String,
    pub session_id: String,
    pub short_id: String,
    pub title: String,
    pub online: bool,
    /// `inject`, `listen`, `prompt_hook`, `queue` or `none`.
    pub channel: String,
}

/// Result of API-LOCAL-009, one entry per attachment.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FetchResponse {
    pub files: Vec<LocalFile>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalFile {
    pub id: uuid::Uuid,
    pub name: String,
    pub size: u64,
    /// Absolute local path once downloaded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Why it isn't here: `expired`, `not-found`, `disk-full`, `slow` or
    /// `failed: <reason>`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
