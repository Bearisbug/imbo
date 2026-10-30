//! WebSocket frames on `/v1/ws` (API-023). JSON text, tagged by `type`.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::api::{Account, MessageId, MessageView, RequestId};

/// Close code: token invalid or revoked; the daemon stops reconnecting.
pub const CLOSE_UNAUTHORIZED: u16 = 4401;
/// Close code: the same device opened a second connection.
pub const CLOSE_REPLACED: u16 = 4409;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct SessionKey {
    pub tool: String,
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PresenceItem {
    pub tool: String,
    pub session_id: String,
    pub title: String,
    pub online: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientFrame {
    Hello {
        client_version: String,
        sessions: Vec<PresenceItem>,
    },
    Presence {
        sessions: Vec<PresenceItem>,
    },
    Ack {
        message_id: MessageId,
        agent_id: Uuid,
        via: String,
    },
    Nack {
        message_id: MessageId,
        agent_id: Uuid,
        reason: String,
    },
    /// Answer to `wake`: the session came online, or `error` says why not.
    Woken {
        agent_id: Uuid,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Delivery {
    pub agent_id: Uuid,
    /// The receiving session on this computer.
    pub tool: String,
    pub session_id: String,
    pub message: MessageView,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Notice {
    /// `contact_request`, `group_invite`, `agent_accepted`, `request_accepted`,
    /// `contact_removed`.
    pub kind: String,
    pub text: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<RequestId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerFrame {
    Welcome {
        account: Account,
        device_id: Uuid,
        min_client_version: String,
    },
    Deliver(Delivery),
    Cancel {
        message_id: MessageId,
        agent_id: Uuid,
    },
    Notice(Notice),
    /// Open this closed session in the background (FLOW-WAKE).
    Wake {
        agent_id: Uuid,
        tool: String,
        session_id: String,
    },
}
