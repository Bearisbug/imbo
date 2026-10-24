//! `imbo hook prompt`: Claude Code's UserPromptSubmit hook (API-LOCAL-003).
//! Hands unread messages to the session as additional context when live
//! insertion isn't working. Never blocks the prompt: any failure exits 0
//! with no output.

use std::io::Read;
use std::time::Duration;

use imbo_proto::TOOL_CLAUDE;
use imbo_proto::local::LocalRequest;
use serde_json::{Value, json};

use crate::local::Local;

pub async fn run(event: String) -> anyhow::Result<()> {
    if event != "prompt" {
        return Ok(());
    }
    let mut input = String::new();
    let _ = std::io::stdin().read_to_string(&mut input);
    let session_id = serde_json::from_str::<Value>(&input)
        .ok()
        .and_then(|v| v["session_id"].as_str().map(str::to_string))
        .or_else(|| std::env::var("CLAUDE_CODE_SESSION_ID").ok())
        .filter(|s| !s.is_empty());
    let Some(session_id) = session_id else {
        return Ok(());
    };
    let req = LocalRequest::HookPrompt {
        tool: TOOL_CLAUDE.into(),
        session_id,
    };
    let local = Local::new();
    let Ok(Ok(res)) = tokio::time::timeout(
        Duration::from_secs(8),
        local.send_with(&req, Duration::from_secs(1)),
    )
    .await
    else {
        return Ok(());
    };
    if let Some(ctx) = res
        .body
        .get("context")
        .and_then(Value::as_str)
        .filter(|_| res.ok)
    {
        let out = json!({
            "hookSpecificOutput": { "hookEventName": "UserPromptSubmit", "additionalContext": ctx }
        });
        println!("{out}");
    }
    Ok(())
}
