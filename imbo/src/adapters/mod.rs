//! One adapter per tool (ADR-016). Everything Imbo does differently for a
//! tool lives in its adapter; the rest of the client finds adapters through
//! `all()` and never names a tool.

pub mod claude;
pub mod codex;

use std::time::Duration;

use futures::future::BoxFuture;
use imbo_proto::api::MessageView;
use imbo_proto::local::LocalFile;
use serde_json::{Map, Value};

/// Every tool Imbo works with. A session is recognised by the first
/// adapter that claims it, so the order matters when two tools leave the
/// same marks.
static ALL: [&dyn Adapter; 2] = [&claude::CLAUDE, &codex::CODEX];

pub fn all() -> &'static [&'static dyn Adapter] {
    &ALL
}

/// The adapter for a tool ID, `None` for tools Imbo doesn't know.
pub fn get(id: &str) -> Option<&'static dyn Adapter> {
    ALL.iter().copied().find(|a| a.id() == id)
}

/// The session behind this call as `<tool>:<session>`. In the tool process
/// `meta` holds the request's `_meta` objects; a terminal command passes
/// none and is recognised from its environment (ADR-008).
pub fn actor(meta: &[&Map<String, Value>]) -> Option<String> {
    ALL.iter()
        .find_map(|a| a.session(meta).map(|s| format!("{}:{s}", a.id())))
}

/// What a tool process tells the daemon about its session (API-LOCAL-001).
pub struct Registration {
    pub session_id: String,
    pub messaging_socket: Option<String>,
    pub messaging_token: Option<String>,
    pub pid: u32,
}

/// A session an adapter knows of.
pub struct Found {
    pub session_id: String,
    /// Empty when the tool has no title for it (yet).
    pub title: String,
    pub online: bool,
}

/// How an agent hands over pictures pasted into its session (ADR-014). The
/// text goes into the send tool's description.
pub enum Paste {
    /// By number, in send's `images`; the adapter finds the picture.
    Numbered(&'static str),
    /// By the path the tool shows, in send's `files`.
    Path(&'static str),
}

/// A picture the user pasted into a session (ADR-014).
pub struct PastedImage {
    pub media_type: String,
    pub bytes: Vec<u8>,
}

impl PastedImage {
    pub fn ext(&self) -> &'static str {
        match self.media_type.as_str() {
            "image/jpeg" => "jpg",
            "image/gif" => "gif",
            "image/webp" => "webp",
            _ => "png",
        }
    }
}

pub trait Adapter: Send + Sync {
    /// Tool ID, the part before `:` in an actor.
    fn id(&self) -> &'static str;

    /// Name shown to people.
    fn name(&self) -> &'static str;

    /// Session ID of the caller, from the request's `_meta` objects or the
    /// environment; `None` when the call isn't from this tool.
    fn session(&self, meta: &[&Map<String, Value>]) -> Option<String>;

    /// Whether this MCP process runs for this tool; `client` is the client
    /// name sent at initialize.
    fn owns_mcp(&self, client: &str) -> bool;

    /// Environment the daemon keeps: where the tool stores its files.
    fn env_pass(&self) -> &'static [&'static str];

    /// Environment naming one session, which a daemon must not inherit.
    fn env_session(&self) -> &'static [&'static str];

    /// Registration the tool process repeats; `None` when the tool's
    /// sessions only show up in their calls.
    fn announce(&self) -> Option<Registration> {
        None
    }

    /// API-LOCAL-001 in the daemon; `None` when the session can't be found.
    fn register(&self, r: Registration) -> Option<Found> {
        self.adopt(&r.session_id)
    }

    /// A session that called the daemon before discovery found it; `None`
    /// when the session can't be found.
    fn adopt(&self, session_id: &str) -> Option<Found>;

    /// How often the daemon runs `discover`.
    fn poll_every(&self) -> Duration;

    /// Every session of this tool the daemon should know of, with titles.
    /// Sessions left out count as closed; an error keeps the last list.
    fn discover(&self) -> BoxFuture<'_, Result<Vec<Found>, String>>;

    /// How messages reach the session now, for `imbo status`: `inject`,
    /// `prompt_hook` or `queue`.
    fn channel(&self, session_id: &str) -> &'static str;

    /// `Err(reason)` when messages can't be put into the session now; the
    /// delivery goes back to the server with that reason.
    fn ready(&self, _session_id: &str) -> Result<(), &'static str> {
        Ok(())
    }

    /// Put a message into the session, written the way this tool shows it.
    fn deliver<'a>(
        &'a self,
        session_id: &'a str,
        m: &'a MessageView,
        files: &'a [LocalFile],
    ) -> BoxFuture<'a, Result<(), String>>;

    fn paste(&self) -> Paste;

    /// Picture `[Image #n]` pasted into the session; the error is for the
    /// agent.
    fn pasted_image(&self, session_id: &str, n: u32) -> Result<PastedImage, String>;
}

/// `IMBO_FAULT_INJECT=claude-code,codex` (tool IDs) makes those tools'
/// inserts fail and `download` makes attachment downloads fail (tests of
/// the fallbacks).
pub fn fault(tool: &str) -> bool {
    std::env::var("IMBO_FAULT_INJECT")
        .map(|v| v.split(',').any(|t| t.trim() == tool))
        .unwrap_or(false)
}

/// A non-empty environment variable.
fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|s| !s.is_empty())
}
