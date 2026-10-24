//! Line-delimited JSON spoken on `~/.imbo/daemon.sock` (API-LOCAL-001..003).
//! Client, MCP process and daemon are the same binary, so this is not a
//! public contract.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::problem::Problem;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum LocalRequest {
    /// API-LOCAL-001: a Claude Code session's MCP process announces itself.
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
    },
    /// API-LOCAL-003: UserPromptSubmit hook asks for unread messages.
    HookPrompt {
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LocalSession {
    pub tool: String,
    pub session_id: String,
    pub short_id: String,
    pub title: String,
    pub online: bool,
    /// `inject`, `prompt_hook`, `queue` or `none`.
    pub channel: String,
}
