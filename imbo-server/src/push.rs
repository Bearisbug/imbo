//! Phone notifications through Expo's push service (ADR-034, FLOW-APP-PUSH).

use serde_json::{Value, json};
use tracing::{info, warn};
use uuid::Uuid;

use crate::config::Config;
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
    from_avatar: Option<Uuid>,
    body: String,
    conversation_id: i64,
    session: Option<Uuid>,
    session_short: Option<String>,
    session_title: Option<String>,
    computer: Option<String>,
    group_id: Option<i32>,
    group_name: Option<String>,
    group_avatar: Option<Uuid>,
    /// A group message whose `@` named this person (RULE-015).
    mention: bool,
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
    // terminal, the web or the App and what someone they block sent. A
    // group message lands in an inbox when its `@` named that member, who
    // gets that one notification instead of the group's (ADR-057). Nothing
    // for a conversation the person muted, but for that `@` (API-085).
    let recipients: Vec<Recipient> = sqlx::query_as(
        "SELECT d.account_id, m.from_account, fa.name AS from_name,
                fa.avatar_id AS from_avatar, m.body, m.conversation_id,
                m.from_agent_id AS session, ag.short_id AS session_short,
                coalesce(ag.custom_title, ag.title) AS session_title, dv.name AS computer,
                g.id AS group_id, g.name AS group_name, g.avatar_id AS group_avatar, g.id IS NOT NULL AS mention
         FROM deliveries d
         JOIN agents t ON t.id = d.agent_id AND t.tool = 'terminal'
         JOIN messages m ON m.id = d.message_id
         JOIN conversations cv ON cv.id = m.conversation_id
         LEFT JOIN chat_groups g ON g.id = cv.group_id
         JOIN accounts fa ON fa.id = m.from_account
         LEFT JOIN agents ag ON ag.id = m.from_agent_id
         LEFT JOIN devices dv ON dv.id = ag.device_id
         WHERE d.message_id = $1
           AND NOT (m.from_account = d.account_id AND m.from_agent_id IS NULL)
           AND (g.id IS NOT NULL OR NOT EXISTS (SELECT 1 FROM conversation_prefs cp
                WHERE cp.account_id = d.account_id AND cp.conversation_id = m.conversation_id AND cp.muted))
         UNION ALL
         SELECT gm.account_id, m.from_account, fa.name,
                fa.avatar_id, m.body, m.conversation_id,
                m.from_agent_id, ag.short_id, coalesce(ag.custom_title, ag.title), dv.name,
                g.id, g.name, g.avatar_id, false
         FROM messages m
         JOIN conversations cv ON cv.id = m.conversation_id
         JOIN chat_groups g ON g.id = cv.group_id
         JOIN group_members gm ON gm.group_id = g.id
         JOIN accounts fa ON fa.id = m.from_account
         LEFT JOIN agents ag ON ag.id = m.from_agent_id
         LEFT JOIN devices dv ON dv.id = ag.device_id
         WHERE m.id = $1
           AND NOT (m.from_account = gm.account_id AND m.from_agent_id IS NULL)
           AND NOT EXISTS (SELECT 1 FROM blocks b WHERE b.account_id = gm.account_id AND b.blocked_id = m.from_account)
           AND NOT EXISTS (SELECT 1 FROM deliveries d JOIN agents t ON t.id = d.agent_id AND t.tool = 'terminal'
                           WHERE d.message_id = m.id AND d.account_id = gm.account_id)
           AND NOT EXISTS (SELECT 1 FROM conversation_prefs cp
                           WHERE cp.account_id = gm.account_id AND cp.conversation_id = m.conversation_id AND cp.muted)",
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
        let phones: Vec<(Uuid, String, Option<String>)> = sqlx::query_as(
            "SELECT id, push_token, locale FROM app_sessions
             WHERE account_id = $1 AND push_token IS NOT NULL AND revoked_at IS NULL AND expires_at > now()",
        )
        .bind(r.account_id)
        .fetch_all(&st.db)
        .await?;
        // Only a mention adds words of our own, in each App's language.
        let mut by_locale: Vec<(Option<String>, Vec<(Uuid, String)>)> = vec![];
        for (app, token, locale) in phones {
            let locale = locale.filter(|_| r.mention);
            match by_locale.iter_mut().find(|(l, _)| *l == locale) {
                Some((_, v)) => v.push((app, token)),
                None => by_locale.push((locale, vec![(app, token)])),
            }
        }
        for (locale, phones) in by_locale {
            send(
                st,
                id,
                r.account_id,
                &phones,
                &note(&st.cfg, &r, &files, locale.as_deref()),
            )
            .await?;
        }
    }
    Ok(())
}

/// The sender, or the group, as the App shows it beside the notification (ADR-064).
fn who(cfg: &Config, id: i64, name: &str, avatar: Option<Uuid>) -> Value {
    let mut v = json!({ "id": id, "name": name });
    if let Some(url) = crate::avatar::url_of(cfg, avatar) {
        v["avatar_url"] = json!(url);
    }
    v
}

/// `locale` is the App's language (API-050), for a mention's words. `data`
/// carries where a reply typed on the notification goes (ADR-064).
fn note(cfg: &Config, r: &Recipient, files: &[String], locale: Option<&str>) -> Note {
    let body = if r.body.trim().is_empty() {
        files.join(", ")
    } else {
        r.body.chars().take(PREVIEW_CHARS).collect()
    };
    let mut data = json!({
        "conversation_id": r.conversation_id,
        "sender": who(cfg, r.from_account.into(), &r.from_name, r.from_avatar),
    });
    // A group's message is titled by the group, the sender up front.
    if let Some(group) = &r.group_name {
        let from = match &r.session_title {
            Some(t) => format!("{} · {t}", r.from_name),
            None => r.from_name.clone(),
        };
        let body = match (r.mention, locale) {
            (false, _) => format!("{from}: {body}"),
            (true, Some("zh-Hans")) => format!("{from} @了你：{body}"),
            (true, _) => format!("{from} mentioned you: {body}"),
        };
        if let Some(id) = r.group_id {
            data["to"] = json!(format!("#{id}"));
            data["group"] = who(cfg, id.into(), group, r.group_avatar);
        }
        return Note {
            title: group.clone(),
            body,
            data,
        };
    }
    // A reply from one of the person's own sessions is titled by the session.
    let own_session = r.from_account == r.account_id;
    let title = match (&r.session_title, &r.computer) {
        (Some(t), Some(c)) if own_session => format!("{t} · {c}"),
        (Some(t), None) if own_session => t.clone(),
        _ => r.from_name.clone(),
    };
    match (own_session, r.session, &r.session_short) {
        // A reply goes back to the session itself (API-043).
        (true, Some(s), _) => data["agent_id"] = json!(s),
        (false, Some(_), Some(short)) => data["to"] = json!(format!("{}/{short}", r.from_account)),
        _ => data["to"] = json!(r.from_account.to_string()),
    }
    Note { title, body, data }
}

/// The notification as Expo gets it, less the phone: a reply action on it, the
/// sender's picture beside it, a chat's notifications stacked together (ADR-064).
fn expo(n: &Note) -> Value {
    json!({
        "title": n.title, "body": n.body, "data": n.data, "sound": "default",
        "categoryId": "message", "mutableContent": true,
        "threadId": n.data["conversation_id"].to_string(),
    })
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
            expo = %expo(n),
            "push (log only)"
        );
        return Ok(());
    }
    let batch: Vec<Value> = phones
        .iter()
        .map(|(_, to)| {
            let mut m = expo(n);
            m["to"] = json!(to);
            m
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
            from_avatar: None,
            body: body.into(),
            conversation_id: 7,
            session: Some(Uuid::nil()),
            session_short: Some("0a1b2c".into()),
            session_title: Some("checkout".into()),
            computer: Some("MacBook".into()),
            group_id: None,
            group_name: None,
            group_avatar: None,
            mention: false,
        }
    }

    fn cfg() -> Config {
        Config {
            domain: Some("api.imbo.dev".into()),
            ..Config::default()
        }
    }

    fn group(mut r: Recipient) -> Recipient {
        r.group_id = Some(1234567);
        r.group_name = Some("release".into());
        r
    }

    #[test]
    fn contact_message_is_titled_by_the_sender() {
        let n = note(&cfg(), &recipient(2, "lunch?"), &[], None);
        assert_eq!(n.title, "bob");
        assert_eq!(n.body, "lunch?");
        assert_eq!(
            n.data,
            json!({ "conversation_id": 7, "sender": { "id": 2, "name": "bob" }, "to": "2/0a1b2c" })
        );
        // The contact themselves, from the web, the App or a terminal: their inbox.
        let mut r = recipient(2, "lunch?");
        r.session = None;
        r.session_short = None;
        assert_eq!(note(&cfg(), &r, &[], None).data["to"], json!("2"));
    }

    #[test]
    fn sender_and_group_pictures_ride_along_unless_missing() {
        let mut r = group(recipient(2, "hi"));
        r.from_avatar = Some(Uuid::nil());
        let d = note(&cfg(), &r, &[], None).data;
        assert_eq!(
            d["sender"]["avatar_url"],
            json!(format!(
                "https://api.imbo.dev/v1/avatars/{}.jpg",
                Uuid::nil()
            ))
        );
        assert_eq!(d["group"], json!({ "id": 1234567, "name": "release" }));
    }

    #[test]
    fn own_session_reply_is_titled_by_session_and_computer() {
        let n = note(&cfg(), &recipient(1, "412 tests pass"), &[], None);
        assert_eq!(n.title, "checkout · MacBook");
        assert_eq!(n.data["agent_id"], json!(Uuid::nil()));
        assert_eq!(n.data.get("to"), None);
    }

    #[test]
    fn group_message_is_titled_by_the_group_with_the_sender_up_front() {
        let n = note(&cfg(), &group(recipient(2, "build is green")), &[], None);
        assert_eq!(n.title, "release");
        assert_eq!(n.body, "bob · checkout: build is green");
        assert_eq!(n.data["to"], json!("#1234567"));
        // The person's own session in the group, sent without a session title.
        let mut r = group(recipient(1, "done"));
        r.session_title = None;
        assert_eq!(note(&cfg(), &r, &[], None).body, "bob: done");
        assert_eq!(note(&cfg(), &r, &[], None).data["to"], json!("#1234567"));
        assert_eq!(note(&cfg(), &r, &[], None).data.get("agent_id"), None);
    }

    #[test]
    fn mention_says_so_in_the_apps_language() {
        let mut r = group(recipient(2, "@alice 看下发布单"));
        r.mention = true;
        let zh = note(&cfg(), &r, &[], Some("zh-Hans"));
        assert_eq!(zh.title, "release");
        assert_eq!(zh.body, "bob · checkout @了你：@alice 看下发布单");
        assert_eq!(
            note(&cfg(), &r, &[], Some("en")).body,
            "bob · checkout mentioned you: @alice 看下发布单"
        );
        assert_eq!(
            note(&cfg(), &r, &[], None).body,
            "bob · checkout mentioned you: @alice 看下发布单"
        );
    }

    #[test]
    fn body_is_cut_to_100_characters_or_names_the_files() {
        let long = "字".repeat(150);
        assert_eq!(
            note(&cfg(), &recipient(2, &long), &[], None)
                .body
                .chars()
                .count(),
            100
        );
        let files = ["a.png".to_string(), "b.log".to_string()];
        assert_eq!(
            note(&cfg(), &recipient(2, " "), &files, None).body,
            "a.png, b.log"
        );
    }

    #[test]
    fn expo_gets_the_reply_category_and_the_chat_as_thread() {
        let n = note(&cfg(), &recipient(2, "lunch?"), &[], None);
        let v = expo(&n);
        assert_eq!(v["categoryId"], json!("message"));
        assert_eq!(v["mutableContent"], json!(true));
        assert_eq!(v["threadId"], json!("7"));
        assert_eq!(v["data"], n.data);
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
