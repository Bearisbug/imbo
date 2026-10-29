//! Phone notifications through Expo's push service (ADR-034, FLOW-APP-PUSH).

use serde_json::{Value, json};
use tracing::{info, warn};
use uuid::Uuid;

use crate::state::St;

const EXPO_PUSH_URL: &str = "https://exp.host/--/api/v2/push/send";
const PREVIEW_CHARS: usize = 100;

/// Notify the recipients' phones about these new messages, in the background.
pub fn messages(st: &St, ids: Vec<i64>) {
    if ids.is_empty() {
        return;
    }
    let st = st.clone();
    tokio::spawn(async move {
        for id in ids {
            if let Err(e) = message(&st, id).await {
                warn!(error = %e, message_id = id, "push");
            }
        }
    });
}

#[derive(sqlx::FromRow)]
struct Recipient {
    account_id: i32,
    from_account: i32,
    from_name: String,
    body: String,
    conversation_id: i64,
    session: Option<Uuid>,
    session_title: Option<String>,
    computer: Option<String>,
    group_name: Option<String>,
}

#[derive(Debug, PartialEq)]
struct Note {
    title: String,
    body: String,
    data: Value,
}

async fn message(st: &St, id: i64) -> Result<(), sqlx::Error> {
    // What lands in a terminal inbox, and a group's messages for each
    // member (ADR-046), minus what the person sent themselves from a
    // terminal, the web or the App and what someone they block sent.
    let recipients: Vec<Recipient> = sqlx::query_as(
        "SELECT d.account_id, m.from_account, fa.name AS from_name, m.body, m.conversation_id,
                m.from_agent_id AS session, coalesce(ag.custom_title, ag.title) AS session_title, dv.name AS computer,
                NULL::text AS group_name
         FROM deliveries d
         JOIN agents t ON t.id = d.agent_id AND t.tool = 'terminal'
         JOIN messages m ON m.id = d.message_id
         JOIN accounts fa ON fa.id = m.from_account
         LEFT JOIN agents ag ON ag.id = m.from_agent_id
         LEFT JOIN devices dv ON dv.id = ag.device_id
         WHERE d.message_id = $1
           AND NOT (m.from_account = d.account_id AND m.from_agent_id IS NULL)
         UNION ALL
         SELECT gm.account_id, m.from_account, fa.name, m.body, m.conversation_id,
                m.from_agent_id, coalesce(ag.custom_title, ag.title), dv.name, g.name
         FROM messages m
         JOIN conversations cv ON cv.id = m.conversation_id
         JOIN chat_groups g ON g.id = cv.group_id
         JOIN group_members gm ON gm.group_id = g.id
         JOIN accounts fa ON fa.id = m.from_account
         LEFT JOIN agents ag ON ag.id = m.from_agent_id
         LEFT JOIN devices dv ON dv.id = ag.device_id
         WHERE m.id = $1
           AND NOT (m.from_account = gm.account_id AND m.from_agent_id IS NULL)
           AND NOT EXISTS (SELECT 1 FROM blocks b WHERE b.account_id = gm.account_id AND b.blocked_id = m.from_account)",
    )
    .bind(id)
    .fetch_all(&st.db)
    .await?;
    if recipients.is_empty() {
        return Ok(());
    }
    let files: Vec<String> =
        sqlx::query_scalar("SELECT name FROM files WHERE message_id = $1 ORDER BY created_at, id")
            .bind(id)
            .fetch_all(&st.db)
            .await?;
    for r in recipients {
        let phones: Vec<(Uuid, String)> = sqlx::query_as(
            "SELECT id, push_token FROM app_sessions
             WHERE account_id = $1 AND push_token IS NOT NULL AND revoked_at IS NULL AND expires_at > now()",
        )
        .bind(r.account_id)
        .fetch_all(&st.db)
        .await?;
        if !phones.is_empty() {
            send(st, id, r.account_id, &phones, &note(&r, &files)).await?;
        }
    }
    Ok(())
}

fn note(r: &Recipient, files: &[String]) -> Note {
    let body = if r.body.trim().is_empty() {
        files.join(", ")
    } else {
        r.body.chars().take(PREVIEW_CHARS).collect()
    };
    // A group's message is titled by the group, the sender up front.
    if let Some(group) = &r.group_name {
        let from = match &r.session_title {
            Some(t) => format!("{} · {t}", r.from_name),
            None => r.from_name.clone(),
        };
        return Note {
            title: group.clone(),
            body: format!("{from}: {body}"),
            data: json!({ "conversation_id": r.conversation_id }),
        };
    }
    // A reply from one of the person's own sessions is titled by the session.
    let own_session = r.from_account == r.account_id;
    let title = match (&r.session_title, &r.computer) {
        (Some(t), Some(c)) if own_session => format!("{t} · {c}"),
        (Some(t), None) if own_session => t.clone(),
        _ => r.from_name.clone(),
    };
    let mut data = json!({ "conversation_id": r.conversation_id });
    if own_session && let Some(s) = r.session {
        data["agent_id"] = json!(s);
    }
    Note { title, body, data }
}

async fn send(
    st: &St,
    message: i64,
    account: i32,
    phones: &[(Uuid, String)],
    n: &Note,
) -> Result<(), sqlx::Error> {
    if st.cfg.push_log_only {
        let sessions: Vec<String> = phones.iter().map(|(id, _)| id.to_string()).collect();
        info!(
            event = "push_logged",
            message_id = message,
            account_id = account,
            sessions = sessions.join(","),
            title = ?n.title,
            body = ?n.body,
            data = %n.data,
            "push (log only)"
        );
        return Ok(());
    }
    let batch: Vec<Value> = phones
        .iter()
        .map(|(_, to)| {
            json!({ "to": to, "title": n.title, "body": n.body, "data": n.data, "sound": "default" })
        })
        .collect();
    let mut req = st.http.post(EXPO_PUSH_URL).json(&batch);
    if let Some(t) = &st.cfg.expo_access_token {
        req = req.bearer_auth(t);
    }
    // One try, 10 seconds; the message is in the App either way (ADR-034).
    let res = match req.send().await {
        Ok(r) => r,
        Err(e) => {
            warn!(error = %e, message_id = message, "push: Expo unreachable");
            return Ok(());
        }
    };
    let status = res.status();
    let v: Value = res.json().await.unwrap_or(Value::Null);
    if !status.is_success() {
        warn!(%status, message_id = message, "push: Expo refused");
        return Ok(());
    }
    let tokens: Vec<&str> = phones.iter().map(|(_, t)| t.as_str()).collect();
    let failed = v["data"]
        .as_array()
        .map_or(0, |t| t.iter().filter(|t| t["status"] == "error").count());
    let gone = unregistered(&v, &tokens);
    if !gone.is_empty() {
        sqlx::query("UPDATE app_sessions SET push_token = NULL WHERE push_token = ANY($1)")
            .bind(&gone)
            .execute(&st.db)
            .await?;
    }
    info!(
        event = "push_sent",
        message_id = message,
        account_id = account,
        tokens = phones.len(),
        failed,
        "push"
    );
    Ok(())
}

/// Tokens Expo answered `DeviceNotRegistered` for; tickets come back in
/// the order the messages were sent.
fn unregistered(v: &Value, tokens: &[&str]) -> Vec<String> {
    let Some(tickets) = v["data"].as_array() else {
        return vec![];
    };
    tickets
        .iter()
        .zip(tokens)
        .filter(|(t, _)| t["status"] == "error" && t["details"]["error"] == "DeviceNotRegistered")
        .map(|(_, token)| token.to_string())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn recipient(from_account: i32, body: &str) -> Recipient {
        Recipient {
            account_id: 1,
            from_account,
            from_name: "bob".into(),
            body: body.into(),
            conversation_id: 7,
            session: Some(Uuid::nil()),
            session_title: Some("checkout".into()),
            computer: Some("MacBook".into()),
            group_name: None,
        }
    }

    #[test]
    fn contact_message_is_titled_by_the_sender() {
        let n = note(&recipient(2, "lunch?"), &[]);
        assert_eq!(n.title, "bob");
        assert_eq!(n.body, "lunch?");
        assert_eq!(n.data, json!({ "conversation_id": 7 }));
    }

    #[test]
    fn own_session_reply_is_titled_by_session_and_computer() {
        let n = note(&recipient(1, "412 tests pass"), &[]);
        assert_eq!(n.title, "checkout · MacBook");
        assert_eq!(n.data["agent_id"], json!(Uuid::nil()));
    }

    #[test]
    fn group_message_is_titled_by_the_group_with_the_sender_up_front() {
        let mut r = recipient(2, "build is green");
        r.group_name = Some("release".into());
        let n = note(&r, &[]);
        assert_eq!(n.title, "release");
        assert_eq!(n.body, "bob · checkout: build is green");
        assert_eq!(n.data, json!({ "conversation_id": 7 }));
        // The person's own session in the group, sent without a session title.
        let mut r = recipient(1, "done");
        r.group_name = Some("release".into());
        r.session_title = None;
        assert_eq!(note(&r, &[]).body, "bob: done");
        assert_eq!(note(&r, &[]).data, json!({ "conversation_id": 7 }));
    }

    #[test]
    fn body_is_cut_to_100_characters_or_names_the_files() {
        let long = "字".repeat(150);
        assert_eq!(note(&recipient(2, &long), &[]).body.chars().count(), 100);
        let files = ["a.png".to_string(), "b.log".to_string()];
        assert_eq!(note(&recipient(2, " "), &files).body, "a.png, b.log");
    }

    #[test]
    fn device_not_registered_tokens_are_picked_out() {
        let v = json!({ "data": [
            { "status": "ok", "id": "x" },
            { "status": "error", "message": "gone", "details": { "error": "DeviceNotRegistered" } },
            { "status": "error", "details": { "error": "MessageRateExceeded" } },
        ]});
        let tokens = [
            "ExponentPushToken[a]",
            "ExponentPushToken[b]",
            "ExponentPushToken[c]",
        ];
        assert_eq!(unregistered(&v, &tokens), vec!["ExponentPushToken[b]"]);
        assert!(unregistered(&Value::Null, &tokens).is_empty());
    }
}
