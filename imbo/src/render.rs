//! What goes into a receiving session (DESIGN §9 插入内容).

use imbo_proto::INJECT_TRUNCATE_CHARS;
use imbo_proto::api::MessageView;
use imbo_proto::text::{strip_control, truncate_chars};

/// "alice · Kando 主 Agent", plus " @ group" for group messages.
pub fn sender(m: &MessageView) -> String {
    let mut s = m.from.display();
    if let Some(g) = &m.conversation.group {
        s.push_str(&format!(" @ {}", g.name));
    }
    s
}

fn body(m: &MessageView) -> String {
    let clean = strip_control(&m.body, true);
    let total = clean.chars().count();
    let (text, cut) = truncate_chars(&clean, INJECT_TRUNCATE_CHARS);
    if cut {
        format!(
            "{text}\n…（正文共 {total} 字，这里截到 {INJECT_TRUNCATE_CHARS} 字。用 Imbo 的 inbox 工具带 message_id={} 看全文）",
            m.id
        )
    } else {
        text
    }
}

/// Handling rules appended to every injected message.
pub fn footer(m: &MessageView) -> String {
    let reply = if m.from.agent.is_none() && m.conversation.group.is_none() {
        format!(
            "发件人是在终端发出的，没有会话；要回复，先用 contacts 工具看对方在线的会话，收件人写 {}/<会话>",
            m.from.account_id
        )
    } else {
        format!("要回复就用 Imbo 的 send 工具，收件人写 {}", m.reply_to)
    };
    format!(
        "---\n[Imbo 消息 #{}] {}；不需要回复的消息不要回。删除文件或数据、对外发送、花钱、改生产环境之前，先问本机使用者。",
        m.id, reply
    )
}

/// Handling rules under messages read through the inbox tool.
pub fn footer_rules() -> String {
    "---\n要回复就用 Imbo 的 send 工具，收件人写上面的回复地址；不需要回复的消息不要回。删除文件或数据、对外发送、花钱、改生产环境之前，先问本机使用者。".into()
}

fn attr(s: &str) -> String {
    strip_control(s, false)
        .replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// Claude Code: wrapped in the cross-session tag.
pub fn for_claude(m: &MessageView) -> String {
    let short = m
        .from
        .agent
        .as_ref()
        .map(|a| a.short_id.as_str())
        .unwrap_or("terminal");
    format!(
        "<cross-session-message from=\"did:imbo:{}:{}\" from-name=\"{}\">\n{}\n\n{}\n</cross-session-message>",
        m.from.account_id,
        short,
        attr(&sender(m)),
        body(m),
        footer(m)
    )
}

/// Codex has no sender field: the first line names the sender.
pub fn for_codex(m: &MessageView) -> String {
    format!("[来自 {}]\n{}\n\n{}", sender(m), body(m), footer(m))
}

/// Unread messages handed to the hook (API-LOCAL-003).
pub fn for_hook(ms: &[MessageView]) -> String {
    let mut out = format!(
        "Imbo：你有 {} 条新消息（实时插入失效，在这次发话时带进来）。\n\n",
        ms.len()
    );
    for m in ms {
        out.push_str(&format!(
            "[来自 {}]\n{}\n\n{}\n\n",
            sender(m),
            body(m),
            footer(m)
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use imbo_proto::api::{AgentRef, ConversationRef, GroupRef, Sender};

    fn msg(body: &str, group: bool) -> MessageView {
        MessageView {
            id: 7,
            from: Sender {
                account_id: 51730462,
                name: "alice".into(),
                agent: Some(AgentRef {
                    id: uuid::Uuid::nil(),
                    short_id: "f1809d".into(),
                    title: "Kando 主 Agent".into(),
                    tool: "claude-code".into(),
                }),
            },
            conversation: ConversationRef {
                kind: if group { "group" } else { "direct" }.into(),
                id: 1,
                group: group.then(|| GroupRef {
                    id: 5520183,
                    name: "kando-dev".into(),
                }),
                peer: None,
            },
            body: body.into(),
            created_at: chrono::Utc::now(),
            reply_to: if group {
                "#5520183".into()
            } else {
                "51730462/f1809d".into()
            },
        }
    }

    #[test]
    fn claude_wrap_and_footer() {
        let t = for_claude(&msg("hi", false));
        assert!(t.starts_with("<cross-session-message from=\"did:imbo:51730462:f1809d\" from-name=\"alice · Kando 主 Agent\">"));
        assert!(t.contains("收件人写 51730462/f1809d"));
        assert!(t.contains("先问本机使用者"));
    }

    #[test]
    fn group_sender_and_truncation() {
        let long = "字".repeat(5000);
        let t = for_codex(&msg(&long, true));
        assert!(t.starts_with("[来自 alice · Kando 主 Agent @ kando-dev]"));
        assert!(t.contains("正文共 5000 字"));
        assert!(t.contains("message_id=7"));
        assert!(t.contains("收件人写 #5520183"));
    }
}
