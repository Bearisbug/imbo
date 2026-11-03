//! Android 推送：Firebase Cloud Messaging HTTP v1。
//!
//! 服务端原来的推送只走 Expo（iOS 与 Expo 写的 App）。原生安卓拿到的是 FCM
//! 注册令牌，`ExponentPushToken[...]` 那套接口发不出去，所以这里补一条通道：
//! 用服务账号的 RS256 私钥换 OAuth2 access token，再 POST 到 FCM v1。
//! 令牌类型靠前缀区分，Expo 的老路径完全不变（ADR-034 仍然成立）。

use std::time::{Duration, Instant};

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::rand::SystemRandom;
use ring::signature::{RSA_PKCS1_SHA256, RsaKeyPair};
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tracing::{info, warn};
use uuid::Uuid;

use crate::config::Fcm;
use crate::push::Note;

const SCOPE: &str = "https://www.googleapis.com/auth/firebase.messaging";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
/// Google 的 access token 一小时有效；提前五分钟换新的，别卡在过期边界上。
const TOKEN_TTL: Duration = Duration::from_secs(55 * 60);

pub struct Sender {
    project_id: String,
    client_email: String,
    key: RsaKeyPair,
    random: SystemRandom,
    token: Mutex<Option<(String, Instant)>>,
}

impl Sender {
    pub fn new(cfg: &Fcm) -> anyhow::Result<Self> {
        let key = RsaKeyPair::from_pkcs8(&cfg.key).map_err(|e| {
            anyhow::anyhow!("IMBO_FCM_PRIVATE_KEY is not an RSA service-account key: {e}")
        })?;
        Ok(Self {
            project_id: cfg.project_id.clone(),
            client_email: cfg.client_email.clone(),
            key,
            random: SystemRandom::new(),
            token: Mutex::new(None),
        })
    }

    /// 发一条通知给这些 FCM 令牌；拿不到 access token 时只记日志，不阻塞消息投递。
    pub async fn send(
        &self,
        http: &reqwest::Client,
        db: &sqlx::PgPool,
        message: i64,
        account: i32,
        phones: &[(Uuid, String)],
        n: &Note,
    ) -> Result<(), sqlx::Error> {
        let Some(access) = self.access_token(http).await else {
            warn!(message_id = message, "push: FCM access token unavailable");
            return Ok(());
        };
        let url = format!(
            "https://fcm.googleapis.com/v1/projects/{}/messages:send",
            self.project_id
        );
        let mut gone: Vec<String> = vec![];
        let mut sent = 0usize;
        for (_, token) in phones {
            let body = json!({ "message": {
                "token": token,
                "notification": { "title": n.title, "body": n.body },
                // FCM 的 data 只能是字符串；客户端按 conversation_id 打开会话
                "data": string_map(&n.data),
                "android": {
                    "priority": "high",
                    "collapse_key": collapse_key(&n.data),
                    "notification": { "channel_id": "imbo", "sound": "default" },
                },
            }});
            match http
                .post(&url)
                .bearer_auth(&access)
                .json(&body)
                .send()
                .await
            {
                Ok(res) if res.status().is_success() => sent += 1,
                Ok(res) if res.status() == reqwest::StatusCode::NOT_FOUND => {
                    // UNREGISTERED：App 被卸载或令牌轮换，服务端把它清掉
                    gone.push(token.clone());
                }
                Ok(res) => {
                    let status = res.status();
                    let text = res.text().await.unwrap_or_default();
                    warn!(%status, message_id = message, "push: FCM refused: {text}");
                }
                Err(e) => warn!(error = %e, message_id = message, "push: FCM unreachable"),
            }
        }
        if !gone.is_empty() {
            sqlx::query("UPDATE app_sessions SET push_token = NULL WHERE push_token = ANY($1)")
                .bind(&gone)
                .execute(db)
                .await?;
        }
        info!(
            event = "push_sent",
            provider = "fcm",
            message_id = message,
            account_id = account,
            tokens = sent,
            failed = phones.len() - sent,
            "push"
        );
        Ok(())
    }

    /// 换 OAuth2 access token：JWT 断言 → token endpoint；失败返回 None（下次再试）。
    async fn access_token(&self, http: &reqwest::Client) -> Option<String> {
        let mut guard = self.token.lock().await;
        if let Some((token, at)) = guard.as_ref()
            && at.elapsed() < TOKEN_TTL
        {
            return Some(token.clone());
        }
        let assertion = self.assertion()?;
        // reqwest 没开 form feature，这里手写 urlencoded 正文：
        // assertion 是 base64url，只有 [A-Za-z0-9-_.]，不需要再转义。
        let body = format!(
            "grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Ajwt-bearer&assertion={assertion}"
        );
        let res = http
            .post(TOKEN_URL)
            .header("Content-Type", "application/x-www-form-urlencoded")
            .body(body)
            .send()
            .await
            .ok()?;
        let status = res.status();
        let v: Value = res.json().await.ok()?;
        if !status.is_success() {
            warn!(%status, "push: FCM token endpoint refused");
            return None;
        }
        let token = v["access_token"].as_str()?.to_string();
        *guard = Some((token.clone(), Instant::now()));
        Some(token)
    }

    /// RS256 签的 JWT 断言（服务账号用的就是这套）。
    fn assertion(&self) -> Option<String> {
        let now = chrono::Utc::now().timestamp();
        let header = json!({ "alg": "RS256", "typ": "JWT" });
        let claims = jwt_claims(&self.client_email, now);
        let head = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).ok()?);
        let body = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).ok()?);
        let signing_input = format!("{head}.{body}");
        let mut sig = vec![0u8; self.key.public().modulus_len()];
        self.key
            .sign(
                &RSA_PKCS1_SHA256,
                &self.random,
                signing_input.as_bytes(),
                &mut sig,
            )
            .ok()?;
        Some(format!("{signing_input}.{}", URL_SAFE_NO_PAD.encode(sig)))
    }
}

/// JWT 的 claims：Google 用 iss/scope/aud 换 access token。
fn jwt_claims(client_email: &str, now: i64) -> Value {
    json!({
        "iss": client_email,
        "scope": SCOPE,
        "aud": TOKEN_URL,
        "iat": now,
        "exp": now + 3600,
    })
}

/// `data` 的值必须是字符串，否则 FCM 直接回 400。
fn string_map(data: &Value) -> Value {
    let mut out = serde_json::Map::new();
    if let Some(obj) = data.as_object() {
        for (k, v) in obj {
            out.insert(
                k.clone(),
                Value::String(match v {
                    Value::String(s) => s.clone(),
                    other => other.to_string(),
                }),
            );
        }
    }
    Value::Object(out)
}

/// 同一个会话的通知互相覆盖，不在通知栏堆一长串。
fn collapse_key(data: &Value) -> String {
    match data.get("conversation_id").and_then(Value::as_i64) {
        Some(id) => format!("conversation-{id}"),
        None => "imbo".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claims_carry_the_service_account_issuer_and_scope() {
        let claims = jwt_claims("svc@demo.iam.gserviceaccount.com", 1_700_000_000);
        assert_eq!(claims["iss"], "svc@demo.iam.gserviceaccount.com");
        assert_eq!(claims["aud"], TOKEN_URL);
        assert_eq!(claims["scope"], SCOPE);
        assert_eq!(claims["iat"], json!(1_700_000_000i64));
        assert_eq!(claims["exp"], json!(1_700_003_600i64));
    }

    #[test]
    fn data_values_become_strings() {
        let out = string_map(&json!({ "conversation_id": 7, "text": "hi", "nested": { "a": 1 } }));
        assert_eq!(out["conversation_id"], json!("7"));
        assert_eq!(out["text"], json!("hi"));
        assert_eq!(out["nested"], json!("{\"a\":1}"));
    }

    #[test]
    fn notifications_for_one_conversation_share_a_collapse_key() {
        assert_eq!(
            collapse_key(&json!({ "conversation_id": 42 })),
            "conversation-42"
        );
        assert_eq!(collapse_key(&json!({})), "imbo");
    }
}
