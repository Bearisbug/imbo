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
    /// Absent when the web app asks.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device: Option<DeviceRef>,
    /// Absent from servers older than 0.2.7.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access: Option<AccessView>,
}

/// Whether the account may send and receive (RULE-007).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AccessView {
    /// `free`, `trial`, `paid` or `expired`.
    pub state: String,
    /// End of the trial, paid-through date, or when access ended; absent for `free`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until: Option<DateTime<Utc>>,
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
    /// False when the contact's trial ended without a subscription (RULE-008).
    #[serde(default = "yes")]
    pub active: bool,
}

fn yes() -> bool {
    true
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_online_at: Option<DateTime<Utc>>,
    /// Messages this session sent the terminal inbox that are still unread.
    #[serde(default)]
    pub unread: i64,
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
    /// Group sends only: other members skipped because their account is
    /// inactive (RULE-008).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inactive_members: Option<u32>,
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
            None => format!("{} · terminal", self.name),
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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<FileView>,
    /// The receiving session of a direct message; `terminal` for the terminal inbox.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to: Option<AgentRef>,
    /// The reader sent it in person, from a terminal or the web app, not
    /// from a session (RULE-012).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub from_me: bool,
    /// Delivery state to `to`, on what the person sent their own session
    /// (API-036, API-043): `pending`, `pushed`, `delivered` or `canceled`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
}

/// An attachment as messages and uploads show it (DESIGN §8).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FileView {
    pub id: Uuid,
    pub name: String,
    pub size: u64,
    /// 64 lowercase hex digits.
    pub sha256: String,
    pub expires_at: DateTime<Utc>,
}

/// Today's upload use (API-026).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Quota {
    pub used: u64,
    pub limit: u64,
    pub resets_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UploadResponse {
    pub file: FileView,
    pub quota: Quota,
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

// ---- web (API-028–031, API-036) ----

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebLoginRequest {
    pub email: String,
    pub code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebLoginResponse {
    pub account: Account,
    pub new_account: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LastMessage {
    pub id: MessageId,
    pub from: Sender,
    /// The first 100 characters of the body, or the file names when the
    /// body is empty.
    pub preview: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationItem {
    pub id: i64,
    /// `direct` or `group`.
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peer: Option<AccountRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<GroupRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_message: Option<LastMessage>,
    /// Messages to me here not yet in a session or read in the terminal.
    pub pending: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConversationsResponse {
    pub conversations: Vec<ConversationItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessagesPage {
    pub messages: Vec<MessageView>,
    pub has_more: bool,
}

/// API-044: how many replies were marked read.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReadResponse {
    pub marked: u64,
}

// ---- billing (API-032–034) ----

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubscriptionView {
    /// `monthly` or `yearly`.
    pub plan: String,
    /// Creem's status as received.
    pub status: String,
    pub paid_until: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BillingResponse {
    pub enabled: bool,
    /// `test` or `live`; absent when Creem is not configured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    pub access: AccessView,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trial_ends_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscription: Option<SubscriptionView>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckoutRequest {
    pub plan: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckoutResponse {
    pub checkout_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortalResponse {
    pub portal_url: String,
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
    /// Missing from servers before group owners (ADR-027).
    #[serde(default)]
    pub owner: Option<AccountRef>,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub billing_webhook_errors_24h: Option<i64>,
}
