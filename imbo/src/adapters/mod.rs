//! One adapter per tool: discover sessions, put messages into them, and find
//! the pictures pasted into them.

pub mod claude;
pub mod codex;

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

/// Image `[Image #n]` pasted into this session; the error is for the agent.
/// Codex names the file of each pasted image in the message, so the agent
/// sends that path as a file instead.
pub fn pasted_image(tool: &str, session_id: &str, n: u32) -> Result<PastedImage, String> {
    match tool {
        imbo_proto::TOOL_CLAUDE => claude::pasted_image(session_id, n).ok_or_else(|| {
            format!("会话记录里找不到粘贴的图片 [Image #{n}]，请让使用者重新粘贴后再发。")
        }),
        _ => Err("这个会话里粘贴的图片，图片标签里带着路径，把路径写进 files。".into()),
    }
}

/// `IMBO_FAULT_INJECT=claude,codex` makes inserts fail and `download` makes
/// attachment downloads fail (tests of the fallbacks).
pub fn fault(tool: &str) -> bool {
    std::env::var("IMBO_FAULT_INJECT")
        .map(|v| v.split(',').any(|t| t.trim() == tool))
        .unwrap_or(false)
}
