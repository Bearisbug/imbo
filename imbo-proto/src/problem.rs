//! RFC 9457 problem envelope and the closed list of error types (DESIGN §14).

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorCode {
    InvalidRequest,
    InvalidEmail,
    CodeInvalid,
    Unauthorized,
    NotLoggedIn,
    Forbidden,
    SignupClosed,
    NotContact,
    NotMember,
    NotOwner,
    RemovedFromGroup,
    NotFound,
    AccountNotFound,
    RecipientNotFound,
    SessionNotFound,
    AlreadyContacts,
    RequestPending,
    RequestClosed,
    EmailTaken,
    AlreadyMember,
    OwnerCannotLeave,
    AmbiguousRecipient,
    CodeExpired,
    NameRequired,
    InvalidName,
    CannotAddSelf,
    UnknownSession,
    BodyInvalid,
    RateLimited,
    CodeLocked,
    EmailSendFailed,
    Unavailable,
    ServerUnreachable,
    DaemonUnavailable,
    InjectFailed,
    FileExpired,
    FileTooLarge,
    FileHashMismatch,
    UploadQuotaExceeded,
    StorageFull,
    FileUnreadable,
    SubscriptionRequired,
    RecipientInactive,
    AlreadySubscribed,
    PaymentProviderFailed,
    ClientOutdated,
    Blocked,
    UnblockFirst,
    AccountBanned,
    AlreadyReported,
    ReportClosed,
    ImageInvalid,
    OtherAccount,
    AppStoreRenewing,
}

impl ErrorCode {
    pub const ALL: [ErrorCode; 54] = [
        Self::InvalidRequest,
        Self::InvalidEmail,
        Self::CodeInvalid,
        Self::Unauthorized,
        Self::NotLoggedIn,
        Self::Forbidden,
        Self::SignupClosed,
        Self::NotContact,
        Self::NotMember,
        Self::NotOwner,
        Self::RemovedFromGroup,
        Self::NotFound,
        Self::AccountNotFound,
        Self::RecipientNotFound,
        Self::SessionNotFound,
        Self::AlreadyContacts,
        Self::RequestPending,
        Self::RequestClosed,
        Self::EmailTaken,
        Self::AlreadyMember,
        Self::OwnerCannotLeave,
        Self::AmbiguousRecipient,
        Self::CodeExpired,
        Self::NameRequired,
        Self::InvalidName,
        Self::CannotAddSelf,
        Self::UnknownSession,
        Self::BodyInvalid,
        Self::RateLimited,
        Self::CodeLocked,
        Self::EmailSendFailed,
        Self::Unavailable,
        Self::ServerUnreachable,
        Self::DaemonUnavailable,
        Self::InjectFailed,
        Self::FileExpired,
        Self::FileTooLarge,
        Self::FileHashMismatch,
        Self::UploadQuotaExceeded,
        Self::StorageFull,
        Self::FileUnreadable,
        Self::SubscriptionRequired,
        Self::RecipientInactive,
        Self::AlreadySubscribed,
        Self::PaymentProviderFailed,
        Self::ClientOutdated,
        Self::Blocked,
        Self::UnblockFirst,
        Self::AccountBanned,
        Self::AlreadyReported,
        Self::ReportClosed,
        Self::ImageInvalid,
        Self::OtherAccount,
        Self::AppStoreRenewing,
    ];

    pub fn slug(self) -> &'static str {
        match self {
            Self::InvalidRequest => "invalid-request",
            Self::InvalidEmail => "invalid-email",
            Self::CodeInvalid => "code-invalid",
            Self::Unauthorized => "unauthorized",
            Self::NotLoggedIn => "not-logged-in",
            Self::Forbidden => "forbidden",
            Self::SignupClosed => "signup-closed",
            Self::NotContact => "not-contact",
            Self::NotMember => "not-member",
            Self::NotOwner => "not-owner",
            Self::RemovedFromGroup => "removed-from-group",
            Self::NotFound => "not-found",
            Self::AccountNotFound => "account-not-found",
            Self::RecipientNotFound => "recipient-not-found",
            Self::SessionNotFound => "session-not-found",
            Self::AlreadyContacts => "already-contacts",
            Self::RequestPending => "request-pending",
            Self::RequestClosed => "request-closed",
            Self::EmailTaken => "email-taken",
            Self::AlreadyMember => "already-member",
            Self::OwnerCannotLeave => "owner-cannot-leave",
            Self::AmbiguousRecipient => "ambiguous-recipient",
            Self::CodeExpired => "code-expired",
            Self::NameRequired => "name-required",
            Self::InvalidName => "invalid-name",
            Self::CannotAddSelf => "cannot-add-self",
            Self::UnknownSession => "unknown-session",
            Self::BodyInvalid => "body-invalid",
            Self::RateLimited => "rate-limited",
            Self::CodeLocked => "code-locked",
            Self::EmailSendFailed => "email-send-failed",
            Self::Unavailable => "unavailable",
            Self::ServerUnreachable => "server-unreachable",
            Self::DaemonUnavailable => "daemon-unavailable",
            Self::InjectFailed => "inject-failed",
            Self::FileExpired => "file-expired",
            Self::FileTooLarge => "file-too-large",
            Self::FileHashMismatch => "file-hash-mismatch",
            Self::UploadQuotaExceeded => "upload-quota-exceeded",
            Self::StorageFull => "storage-full",
            Self::FileUnreadable => "file-unreadable",
            Self::SubscriptionRequired => "subscription-required",
            Self::RecipientInactive => "recipient-inactive",
            Self::AlreadySubscribed => "already-subscribed",
            Self::PaymentProviderFailed => "payment-provider-failed",
            Self::ClientOutdated => "client-outdated",
            Self::Blocked => "blocked",
            Self::UnblockFirst => "unblock-first",
            Self::AccountBanned => "account-banned",
            Self::AlreadyReported => "already-reported",
            Self::ReportClosed => "report-closed",
            Self::ImageInvalid => "image-invalid",
            Self::OtherAccount => "other-account",
            Self::AppStoreRenewing => "app-store-renewing",
        }
    }

    pub fn type_uri(self) -> String {
        format!("/errors/{}", self.slug())
    }

    pub fn from_type(t: &str) -> Option<Self> {
        let slug = t.strip_prefix("/errors/")?;
        Self::ALL.into_iter().find(|c| c.slug() == slug)
    }

    pub fn status(self) -> u16 {
        match self {
            Self::InvalidRequest
            | Self::InvalidEmail
            | Self::CodeInvalid
            | Self::FileUnreadable => 400,
            Self::Unauthorized | Self::NotLoggedIn => 401,
            Self::SubscriptionRequired => 402,
            Self::Forbidden
            | Self::SignupClosed
            | Self::NotContact
            | Self::NotMember
            | Self::NotOwner
            | Self::RemovedFromGroup
            | Self::RecipientInactive
            | Self::Blocked
            | Self::UnblockFirst
            | Self::AccountBanned => 403,
            Self::NotFound
            | Self::AccountNotFound
            | Self::RecipientNotFound
            | Self::SessionNotFound => 404,
            Self::AlreadyContacts
            | Self::RequestPending
            | Self::RequestClosed
            | Self::EmailTaken
            | Self::AlreadyMember
            | Self::OwnerCannotLeave
            | Self::AmbiguousRecipient
            | Self::AlreadySubscribed
            | Self::AlreadyReported
            | Self::ReportClosed
            | Self::OtherAccount
            | Self::AppStoreRenewing => 409,
            Self::CodeExpired | Self::FileExpired => 410,
            Self::FileTooLarge => 413,
            Self::ClientOutdated => 426,
            Self::NameRequired
            | Self::InvalidName
            | Self::CannotAddSelf
            | Self::UnknownSession
            | Self::BodyInvalid
            | Self::FileHashMismatch
            | Self::ImageInvalid => 422,
            Self::RateLimited | Self::CodeLocked | Self::UploadQuotaExceeded => 429,
            Self::EmailSendFailed | Self::PaymentProviderFailed => 502,
            Self::Unavailable | Self::ServerUnreachable | Self::DaemonUnavailable => 503,
            Self::InjectFailed => 500,
            Self::StorageFull => 507,
        }
    }

    pub fn title(self) -> &'static str {
        match self {
            Self::InvalidRequest => "Invalid request",
            Self::InvalidEmail => "Invalid email address",
            Self::CodeInvalid => "Wrong code",
            Self::Unauthorized => "Not signed in on this computer",
            Self::NotLoggedIn => "Not logged in",
            Self::Forbidden => "Only the account owner can do this",
            Self::SignupClosed => "Sign-up is closed for this email",
            Self::NotContact => "Not a contact",
            Self::NotMember => "Not a group member",
            Self::NotOwner => "Not the group owner",
            Self::RemovedFromGroup => "Removed from this group",
            Self::NotFound => "Not found",
            Self::AccountNotFound => "No such account",
            Self::RecipientNotFound => "Recipient not found",
            Self::SessionNotFound => "Session not found",
            Self::AlreadyContacts => "Already contacts",
            Self::RequestPending => "Request already pending",
            Self::RequestClosed => "Request already handled",
            Self::EmailTaken => "Email already in use",
            Self::AlreadyMember => "Already a member",
            Self::OwnerCannotLeave => "The group owner can't leave",
            Self::AmbiguousRecipient => "Recipient is ambiguous",
            Self::CodeExpired => "Code expired",
            Self::NameRequired => "Name required",
            Self::InvalidName => "Invalid name",
            Self::CannotAddSelf => "Cannot add yourself",
            Self::UnknownSession => "Unknown sending session",
            Self::BodyInvalid => "Invalid message body",
            Self::RateLimited => "Too many requests",
            Self::CodeLocked => "Too many wrong codes",
            Self::EmailSendFailed => "Could not send email",
            Self::Unavailable => "Server unavailable",
            Self::ServerUnreachable => "Cannot reach the Imbo server",
            Self::DaemonUnavailable => "Imbo daemon is not running",
            Self::InjectFailed => "Could not deliver into the session",
            Self::FileExpired => "File expired",
            Self::FileTooLarge => "File too large",
            Self::FileHashMismatch => "File changed during upload",
            Self::UploadQuotaExceeded => "Daily upload limit reached",
            Self::StorageFull => "Server storage is full",
            Self::FileUnreadable => "Cannot read the file",
            Self::SubscriptionRequired => "Subscription required",
            Self::RecipientInactive => "Recipient account is inactive",
            Self::AlreadySubscribed => "Already subscribed",
            Self::PaymentProviderFailed => "Payment provider unavailable",
            Self::ClientOutdated => "Imbo needs an update",
            Self::Blocked => "This person doesn't accept your messages",
            Self::UnblockFirst => "You blocked this person",
            Self::AccountBanned => "This account is banned",
            Self::AlreadyReported => "Already reported",
            Self::ReportClosed => "Report already handled",
            Self::ImageInvalid => "Not a JPEG or PNG image",
            Self::OtherAccount => "Subscription belongs to another account",
            Self::AppStoreRenewing => "App Store subscription still renews",
        }
    }
}

/// One option offered when a recipient is ambiguous or incomplete.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Candidate {
    /// Address that can be used as `to` verbatim.
    pub to: String,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub online: Option<bool>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Problem {
    #[serde(rename = "type")]
    pub type_: String,
    pub title: String,
    pub status: u16,
    pub detail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instance: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidates: Option<Vec<Candidate>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry_after_s: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempts_left: Option<u32>,
}

impl Problem {
    pub fn new(code: ErrorCode, detail: impl Into<String>) -> Self {
        Self {
            type_: code.type_uri(),
            title: code.title().to_string(),
            status: code.status(),
            detail: detail.into(),
            instance: None,
            candidates: None,
            retry_after_s: None,
            attempts_left: None,
        }
    }

    pub fn code(&self) -> Option<ErrorCode> {
        ErrorCode::from_type(&self.type_)
    }

    pub fn is(&self, code: ErrorCode) -> bool {
        self.code() == Some(code)
    }

    pub fn with_candidates(mut self, c: Vec<Candidate>) -> Self {
        self.candidates = Some(c);
        self
    }

    pub fn with_retry_after(mut self, s: u64) -> Self {
        self.retry_after_s = Some(s);
        self
    }

    /// The `Imbo-Request-Id` this problem belongs to, if the server set `instance`.
    pub fn request_id(&self) -> Option<&str> {
        self.instance.as_deref()?.strip_prefix("/requests/")
    }
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.detail)
    }
}

impl std::error::Error for Problem {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs_round_trip() {
        for c in ErrorCode::ALL {
            assert_eq!(ErrorCode::from_type(&c.type_uri()), Some(c));
        }
    }
}
