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
    /// Absent without an avatar (API-045).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_url: Option<String>,
    /// Only on an admin's own account (ROLE-Admin, API-004).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub admin: bool,
}

/// An account or group with its picture: `avatar_url` is absent without one
/// or while the account is banned (DESIGN §8).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Profile {
    pub id: i32,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_url: Option<String>,
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

/// API-076: the new email and the code API-075 sent to it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmailChangeRequest {
    pub email: String,
    pub code: String,
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
    /// Exactly one of `account_id` and `email` (API-008).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<AccountId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
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
    /// False when the contact's trial ended without a subscription
    /// (RULE-008), or the account is banned (RULE-014).
    #[serde(default = "yes")]
    pub active: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_url: Option<String>,
    /// I blocked this contact (RULE-013).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub blocked: bool,
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
    /// The name I gave the session, else the tool's title (ADR-045).
    pub title: String,
    /// The tool's own title.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_title: Option<String>,
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
    /// A message from me in the App or on the web opens it on its computer
    /// (FLOW-WAKE).
    #[serde(default)]
    pub wakeable: bool,
    /// The last try to open it, while it is still offline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wake: Option<WakeView>,
}

/// How opening an offline session on its computer went (API-014).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WakeView {
    /// `waking` or `failed`.
    pub state: String,
    /// Why it failed, for the person to read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentsResponse {
    pub agents: Vec<Agent>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentResponse {
    pub agent: Agent,
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
    /// On a group message: the reader's other sessions that got it too, to
    /// talk it over with before replying (ADR-052).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub siblings: Vec<Sibling>,
}

/// One of the reader's own sessions, as `siblings` lists it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Sibling {
    /// Address to message it: `<account ID>/<short ID>`.
    pub to: String,
    pub title: String,
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

// ---- App (API-048–052) ----

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppLoginRequest {
    pub email: String,
    pub code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// `ios` or `android`.
    pub platform: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppLoginResponse {
    pub account: Account,
    /// Shown only here; the server keeps its SHA-256 (ADR-033).
    pub token: String,
    pub new_account: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PushTokenRequest {
    /// An Expo push token, `ExponentPushToken[…]`.
    pub push_token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeleteAccountRequest {
    /// Must be `DELETE`.
    pub confirm: String,
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
    pub peer: Option<Profile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<Profile>,
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
    /// `active`, `past_due`, `scheduled_cancel` or `canceled` (API-035).
    pub status: String,
    pub paid_until: DateTime<Utc>,
    /// `waffo` or `apple`: where it is managed.
    pub provider: String,
}

/// What the App needs to sell App Store subscriptions (API-032).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppStoreView {
    pub product_monthly: String,
    pub product_yearly: String,
    /// The appAccountToken the App passes to Apple with a purchase.
    pub account_token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BillingResponse {
    pub enabled: bool,
    /// `test` or `live`; absent when Waffo is not configured.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    pub access: AccessView,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trial_ends_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscription: Option<SubscriptionView>,
    /// Only when the server sells App Store subscriptions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_store: Option<AppStoreView>,
}

/// API-071.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppStoreTransactionRequest {
    /// The StoreKit 2 transaction JWS.
    pub signed_transaction: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppStoreTransactionResponse {
    pub subscription: SubscriptionView,
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
    pub members: Vec<MemberRef>,
    pub my_sessions: Vec<AgentRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_url: Option<String>,
}

/// A group member as API-019 lists them.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemberRef {
    pub id: AccountId,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_url: Option<String>,
    /// Same as [`ContactItem::active`].
    #[serde(default = "yes")]
    pub active: bool,
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
    /// Hours the oldest open report has waited; absent when none is open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oldest_open_report_h: Option<f64>,
}

/// API-074: what the website shows; no account or session details.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Stats {
    /// Tool sessions online right now; terminal inboxes don't count.
    pub sessions_live: i64,
}

// ---- avatars, blocks and reports (API-045–061) ----

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AvatarResponse {
    pub avatar_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockRequest {
    pub account_id: AccountId,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockItem {
    pub account: Profile,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockResponse {
    pub block: BlockItem,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlocksResponse {
    pub blocks: Vec<BlockItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportRequest {
    pub account_id: AccountId,
    /// `spam`, `harassment`, `inappropriate_avatar` or `other`.
    pub reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_id: Option<GroupId>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportCreated {
    pub id: i64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportCreatedResponse {
    pub report: ReportCreated,
}

// ---- admin (API-062–070) ----

/// A reporter or reported account as the admin sees it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportParty {
    pub id: AccountId,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_url: Option<String>,
    pub banned: bool,
    /// The reported account's email, in API-063 only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    /// API-063 only: the reported account changed its avatar after the report.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_changed: Option<bool>,
}

/// A file as copied into a report: name and size only.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ReportFile {
    pub name: String,
    pub size: u64,
}

/// One message copied into a report when it was made (ADR-039).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportMessage {
    /// `message` or `request_note`.
    pub kind: String,
    pub from: AccountRef,
    pub body: String,
    pub files: Vec<ReportFile>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportView {
    pub id: i64,
    pub reporter: ReportParty,
    pub target: ReportParty,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group: Option<GroupRef>,
    pub reason: String,
    /// `open`, `actioned` or `dismissed`.
    pub status: String,
    pub created_at: DateTime<Utc>,
    // API-063 only from here on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub messages: Option<Vec<ReportMessage>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportsPage {
    pub reports: Vec<ReportView>,
    pub total: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReportResponse {
    pub report: ReportView,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolveRequest {
    /// `ban`, `remove_avatar` or `dismiss`.
    pub action: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolveResponse {
    pub report: ReportView,
    /// Only for `ban`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscription_canceled: Option<bool>,
    /// Only for `ban`; as in [`BanResponse`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_store_subscription: Option<bool>,
}

/// The latest subscription, as the admin sees it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminSubscription {
    pub status: String,
    pub paid_until: DateTime<Utc>,
    /// `waffo` or `apple`.
    pub provider: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminAccount {
    pub id: AccountId,
    pub name: String,
    pub email: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar_url: Option<String>,
    pub created_at: DateTime<Utc>,
    pub access: AccessView,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subscription: Option<AdminSubscription>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_active_at: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub banned_at: Option<DateTime<Utc>>,
    pub admin: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminAccountsPage {
    pub accounts: Vec<AdminAccount>,
    pub total: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdminAccountResponse {
    pub account: AdminAccount,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BanResponse {
    pub account: AdminAccount,
    pub subscription_canceled: bool,
    /// It has an App Store subscription that still renews; only it can cancel that.
    pub app_store_subscription: bool,
}

/// One Beijing-time day of API-068.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatsDay {
    /// `2026-10-27`.
    pub date: String,
    pub accounts: i64,
    pub active_7d: i64,
    pub messages: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatsResponse {
    pub accounts: i64,
    pub active_7d: i64,
    pub paid: i64,
    pub messages_today: i64,
    pub days: Vec<StatsDay>,
}
