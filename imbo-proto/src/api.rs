//! REST request and response bodies under `/v1` (DESIGN §8).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub type AccountId = i32;
pub type GroupId = i32;
pub type MessageId = i64;
pub type RequestId = i64;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Account {
    pub id: AccountId,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AccountRef {
    pub id: AccountId,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GroupRef {
    pub id: GroupId,
    pub name: String,
}

// ---- auth ----

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeRequest {
    pub email: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodeResponse {
    pub retry_after_s: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyRequest {
    pub email: String,
    pub code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub device_name: String,
    pub platform: String,
    pub client_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyResponse {
    pub account: Account,
    pub device_id: Uuid,
    pub token: String,
    pub new_account: bool,
}

// ---- me & devices ----

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeviceRef {
    pub id: Uuid,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MeResponse {
    pub account: Account,
    pub device: DeviceRef,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenameRequest {
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccountResponse {
    pub account: Account,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Device {
    pub id: Uuid,
    pub name: String,
    pub platform: String,
    pub created_at: DateTime<Utc>,
    pub last_seen_at: Option<DateTime<Utc>>,
    pub current: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DevicesResponse {
    pub devices: Vec<Device>,
}

// ---- contacts & requests ----

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContactRequestBody {
    pub account_id: AccountId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestItem {
    pub id: RequestId,
    pub kind: String,
    pub from: AccountRef,
    pub to: AccountRef,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<GroupRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decided_by: Option<String>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestResponse {
    pub request: RequestItem,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RequestsResponse {
    pub requests: Vec<RequestItem>,
    /// Set when `note` was withheld because an agent is asking.
    #[serde(default)]
    pub notes_hidden: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AcceptBody {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sessions: Option<Vec<Uuid>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AcceptResponse {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contact: Option<AccountRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<GroupRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub my_sessions: Option<Vec<AgentRef>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionInfo {
    pub short_id: String,
    pub title: String,
    pub tool: String,
    pub online: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_online_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContactItem {
    pub id: AccountId,
    pub name: String,
    pub sessions: Vec<SessionInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContactsResponse {
    pub contacts: Vec<ContactItem>,
}

// ---- agents ----

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentRef {
    pub id: Uuid,
    pub short_id: String,
    pub title: String,
    pub tool: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Agent {
    pub id: Uuid,
    pub short_id: String,
    pub session_id: String,
    pub title: String,
    pub tool: String,
    pub online: bool,
    pub device_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentsResponse {
    pub agents: Vec<Agent>,
}

// ---- messages ----

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendBody {
    pub to: String,
    pub body: String,
    pub client_msg_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ConversationRef {
    /// `direct` or `group`.
    pub kind: String,
    pub id: i64,
    /// Group the message went to, for `group` conversations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<GroupRef>,
    /// The other account, for `direct` conversations seen by the sender.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer: Option<AccountRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendResponse {
    pub message_id: MessageId,
    pub conversation: ConversationRef,
    pub recipients: u32,
    /// Went to the peer's terminal inbox instead of a session.
    #[serde(default)]
    pub terminal: bool,
}

/// Who sent a message, as determined by the server.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Sender {
    pub account_id: AccountId,
    pub name: String,
    /// The sending session; absent when the person sent from a terminal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<AgentRef>,
}

impl Sender {
    /// "name · session title", or "name · terminal" for the person.
    pub fn display(&self) -> String {
        match &self.agent {
            Some(a) => format!("{} · {}", self.name, a.title),
            None => format!("{} · 终端", self.name),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageView {
    pub id: MessageId,
    pub from: Sender,
    pub conversation: ConversationRef,
    pub body: String,
    pub created_at: DateTime<Utc>,
    /// Address the recipient can put in `to` to answer.
    pub reply_to: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboxItem {
    pub message: MessageView,
    pub agent: AgentRef,
    pub state: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboxResponse {
    pub items: Vec<InboxItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullResponse {
    pub messages: Vec<MessageView>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageResponse {
    pub message: MessageView,
}

// ---- groups ----

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateGroupBody {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sessions: Option<Vec<Uuid>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupView {
    pub id: GroupId,
    pub name: String,
    pub members: Vec<AccountRef>,
    pub my_sessions: Vec<AgentRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupResponse {
    pub group: GroupView,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GroupsResponse {
    pub groups: Vec<GroupView>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InviteBody {
    pub account_id: AccountId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionsBody {
    pub sessions: Vec<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MySessionsResponse {
    pub my_sessions: Vec<AgentRef>,
}

// ---- health ----

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Health {
    pub status: String,
    pub name: String,
    pub version: String,
    pub db: String,
    pub ws_devices: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stuck_deliveries: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backup_age_h: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_free_pct: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ack_p95_ms: Option<f64>,
}
