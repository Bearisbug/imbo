//! Alipay time packs (API-080, API-083, API-084, ADR-063). The server signs
//! a cashier page for the web app or an order string for the App's Alipay
//! SDK; Alipay notifies the server once paid, and a buyer back from paying
//! hands in the order number to have it looked up.

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::Context;
use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use chrono::{DateTime, FixedOffset, Months, NaiveDateTime, Utc};
use imbo_proto::ErrorCode;
use ring::rand::SystemRandom;
use ring::signature::{RSA_PKCS1_2048_8192_SHA256, RSA_PKCS1_SHA256, UnparsedPublicKey};
use serde_json::{Value, json};
use sqlx::PgConnection;
use tracing::{info, warn};

use super::{Channel, Ctx, Outcome, provider_failed};
use crate::billing;
use crate::config::{Alipay, Config, rsa_key_pair};
use crate::error::{ApiError, ApiResult, err, rate_limited};
use crate::state::St;

/// Alipay answers a lookup within this or it counts as failed.
const TIMEOUT: Duration = Duration::from_secs(5);
/// An order nobody pays closes after this.
const PAY_WITHIN: &str = "1h";

/// One of the two packs: its price decides it.
struct Pack {
    plan: &'static str,
    /// Yuan, as Alipay writes amounts.
    amount: &'static str,
    subject: &'static str,
    months: u32,
}

const PACKS: [Pack; 2] = [
    Pack {
        plan: "monthly",
        amount: "15.00",
        subject: "Imbo 月卡",
        months: 1,
    },
    Pack {
        plan: "yearly",
        amount: "150.00",
        subject: "Imbo 年卡",
        months: 12,
    },
];

fn pack(plan: &str) -> Option<&'static Pack> {
    PACKS.iter().find(|p| p.plan == plan)
}

fn pack_costing(amount: &str) -> Option<&'static Pack> {
    PACKS.iter().find(|p| p.amount == amount)
}

/// Alipay writes times in Beijing time, and a calendar month is counted there.
fn beijing() -> FixedOffset {
    FixedOffset::east_opt(8 * 3600).expect("UTC+8 is a valid offset")
}

/// Alipay time packs on the payment-channel port. `app`: selling to the
/// App, with the mobile app and an order string, not a cashier page. A pack
/// doesn't renew, so there is nothing to stop.
pub struct Adapter<'a> {
    pub alipay: &'a Alipay,
    pub app: bool,
}

#[async_trait]
impl Channel for Adapter<'_> {
    fn name(&self) -> &'static str {
        "alipay"
    }

    fn owns(&self, id: &str) -> bool {
        id.starts_with("ALIPAY_")
    }

    /// API-080: sign a new order. Nothing is asked of Alipay or recorded.
    async fn begin(&self, ctx: &Ctx<'_>, account: i32, plan: &str) -> ApiResult<String> {
        let (Some(pack), Some(app_id)) = (pack(plan), self.alipay.seller(self.app)) else {
            return Err(err(ErrorCode::NotFound, "Nothing is sold here."));
        };
        let now = Utc::now();
        let out_trade_no = format!("ALIPAY_{account}_{}", now.timestamp_millis());
        let (method, product_code) = if self.app {
            ("alipay.trade.app.pay", "QUICK_MSECURITY_PAY")
        } else {
            ("alipay.trade.page.pay", "FAST_INSTANT_TRADE_PAY")
        };
        let biz = json!({
            "out_trade_no": out_trade_no,
            "total_amount": pack.amount,
            "subject": pack.subject,
            "product_code": product_code,
            "timeout_express": PAY_WITHIN,
        });
        let mut params = call(app_id, method, &biz, now);
        if let Some(domain) = &ctx.cfg.domain {
            params.insert(
                "notify_url".into(),
                format!("https://{domain}/v1/billing/alipay/notify"),
            );
        }
        if !self.app
            && let Some(origin) = &ctx.cfg.web_origin
        {
            params.insert("return_url".into(), format!("{origin}/account?paid=alipay"));
        }
        sign(&self.alipay.key, &mut params).ok_or_else(provider_failed)?;
        let signed = encode(&params);
        Ok(if self.app {
            signed
        } else {
            format!("{}?{signed}", self.alipay.gateway())
        })
    }

    /// RB-005: an order whose notification never arrived. Either app may
    /// have made it, so both are asked.
    async fn pull(&self, ctx: &Ctx<'_>, id: &str) -> anyhow::Result<String> {
        let account = account_of(id)
            .with_context(|| format!("{id}: a time pack looks like ALIPAY_<account>_<ms>"))?;
        let mut apps = vec![self.alipay.app_id.as_str()];
        apps.extend(
            self.alipay
                .mobile_app_id
                .as_deref()
                .filter(|m| *m != self.alipay.app_id),
        );
        let mut trade = None;
        for app_id in apps {
            trade = query(ctx, self.alipay, app_id, id).await.map_err(|_| {
                anyhow::anyhow!("{id}: Alipay did not answer; nothing recorded, run this again")
            })?;
            if trade.is_some() {
                break;
            }
        }
        let Some(trade) = trade else {
            anyhow::bail!("{id}: Alipay has no such order; nothing recorded");
        };
        if !paid(&trade.status) {
            anyhow::bail!("{id}: not paid ({}); nothing recorded", trade.status);
        }
        let pack = pack_costing(&trade.amount).with_context(|| {
            format!("{id}: {} yuan is neither pack; not recorded", trade.amount)
        })?;
        let mut tx = ctx.db.begin().await?;
        let recorded = record(
            &mut tx,
            ctx.cfg,
            self.alipay,
            account,
            id,
            pack,
            trade.paid_at,
        )
        .await?;
        tx.commit().await?;
        let Some(r) = recorded else {
            anyhow::bail!("{id}: account {account} no longer exists; not recorded");
        };
        Ok(format!(
            "{id}: {} for account {account} ({}, paid until {})",
            if r.new {
                "recorded"
            } else {
                "already recorded"
            },
            pack.plan,
            r.until.to_rfc3339()
        ))
    }
}

/// The account in an order number `ALIPAY_<account>_<ms>`.
fn account_of(out_trade_no: &str) -> Option<i32> {
    let (account, ms) = out_trade_no.strip_prefix("ALIPAY_")?.split_once('_')?;
    ms.parse::<i64>().ok()?;
    account.parse().ok()
}

/// Paid, as a trade status: successful, or finished (no refund possible).
fn paid(status: &str) -> bool {
    matches!(status, "TRADE_SUCCESS" | "TRADE_FINISHED")
}

/// A time like `2026-11-04 12:00:00`, Beijing time.
fn alipay_time(raw: &str) -> Option<DateTime<Utc>> {
    NaiveDateTime::parse_from_str(raw, "%Y-%m-%d %H:%M:%S")
        .ok()?
        .and_local_timezone(beijing())
        .single()
        .map(|t| t.with_timezone(&Utc))
}

/// A pack runs from payment, or from the end of the trial if that is later
/// (ADR-063), for a calendar month or a year.
fn paid_until(
    pack: &Pack,
    paid_at: DateTime<Utc>,
    trial_end: Option<DateTime<Utc>>,
) -> DateTime<Utc> {
    let start = trial_end.map_or(paid_at, |t| t.max(paid_at));
    (start.with_timezone(&beijing()) + Months::new(pack.months)).with_timezone(&Utc)
}

/// What recording a pack found.
struct Recorded {
    /// What the pack grants access through.
    until: DateTime<Utc>,
    /// Recorded by this call, not an earlier one.
    new: bool,
}

/// Record a paid pack once (INV-033); `None` when the account no longer
/// exists. A pack once recorded is never changed, a revocation included.
async fn record(
    tx: &mut PgConnection,
    cfg: &Config,
    alipay: &Alipay,
    account: i32,
    id: &str,
    pack: &Pack,
    paid_at: DateTime<Utc>,
) -> Result<Option<Recorded>, sqlx::Error> {
    let created_at: Option<DateTime<Utc>> =
        sqlx::query_scalar("SELECT created_at FROM accounts WHERE id = $1")
            .bind(account)
            .fetch_optional(&mut *tx)
            .await?;
    let Some(created_at) = created_at else {
        return Ok(None);
    };
    let until = paid_until(pack, paid_at, billing::trial_end(cfg, created_at));
    let inserted: Option<DateTime<Utc>> = sqlx::query_scalar(
        "INSERT INTO subscriptions
           (id, provider, mode, account_id, plan, status, paid_until, provider_updated_at)
         VALUES ($1, 'alipay', $2, $3, $4, 'scheduled_cancel', $5, now())
         ON CONFLICT (id) DO NOTHING
         RETURNING paid_until",
    )
    .bind(id)
    .bind(alipay.mode())
    .bind(account)
    .bind(pack.plan)
    .bind(until)
    .fetch_optional(&mut *tx)
    .await?;
    if let Some(until) = inserted {
        info!(
            event = "alipay_pack_recorded",
            account_id = account,
            plan = pack.plan,
            paid_until = %until,
            mode = alipay.mode(),
            "alipay time pack recorded"
        );
        return Ok(Some(Recorded { until, new: true }));
    }
    let until = sqlx::query_scalar("SELECT paid_until FROM subscriptions WHERE id = $1")
        .bind(id)
        .fetch_one(&mut *tx)
        .await?;
    Ok(Some(Recorded { until, new: false }))
}

// ---- signing ----

/// The common parameters of a call to the gateway.
fn call(app_id: &str, method: &str, biz: &Value, now: DateTime<Utc>) -> BTreeMap<String, String> {
    let timestamp = now
        .with_timezone(&beijing())
        .format("%Y-%m-%d %H:%M:%S")
        .to_string();
    [
        ("app_id", app_id.to_string()),
        ("method", method.to_string()),
        ("format", "JSON".to_string()),
        ("charset", "utf-8".to_string()),
        ("sign_type", "RSA2".to_string()),
        ("timestamp", timestamp),
        ("version", "1.0".to_string()),
        ("biz_content", biz.to_string()),
    ]
    .into_iter()
    .map(|(k, v)| (k.to_string(), v))
    .collect()
}

/// What Alipay signs: the parameters but `skip` and the empty ones, sorted
/// by name, as `k=v` joined by `&`, the values not encoded.
fn signing_string(params: &BTreeMap<String, String>, skip: &[&str]) -> String {
    params
        .iter()
        .filter(|(k, v)| !v.is_empty() && !skip.contains(&k.as_str()))
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&")
}

/// Add `sign`: RSA2 with the app private key over every parameter but itself.
fn sign(key: &[u8], params: &mut BTreeMap<String, String>) -> Option<()> {
    let key = rsa_key_pair(key)?;
    let mut sig = vec![0; key.public().modulus_len()];
    key.sign(
        &RSA_PKCS1_SHA256,
        &SystemRandom::new(),
        signing_string(params, &["sign"]).as_bytes(),
        &mut sig,
    )
    .ok()?;
    params.insert("sign".into(), STANDARD.encode(sig));
    Some(())
}

fn encode(params: &BTreeMap<String, String>) -> String {
    form_urlencoded::Serializer::new(String::new())
        .extend_pairs(params)
        .finish()
}

/// A notification is signed with RSA2 over every parameter but `sign` and
/// `sign_type`; Alipay's public key verifies it.
fn verified(alipay_key: &[u8], params: &BTreeMap<String, String>) -> bool {
    let Some(sig) = params.get("sign").and_then(|s| STANDARD.decode(s).ok()) else {
        return false;
    };
    UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, alipay_key)
        .verify(
            signing_string(params, &["sign", "sign_type"]).as_bytes(),
            &sig,
        )
        .is_ok()
}

// ---- looking an order up ----

/// An order as `alipay.trade.query` reports it.
struct Trade {
    status: String,
    amount: String,
    /// When it was paid; now when Alipay doesn't say.
    paid_at: DateTime<Utc>,
}

/// `None`: Alipay has no such order, as for one whose cashier was never
/// used. Failing or slower than 5 seconds is 502.
async fn query(
    ctx: &Ctx<'_>,
    alipay: &Alipay,
    app_id: &str,
    out_trade_no: &str,
) -> ApiResult<Option<Trade>> {
    let biz = json!({ "out_trade_no": out_trade_no });
    let mut params = call(app_id, "alipay.trade.query", &biz, Utc::now());
    sign(&alipay.key, &mut params).ok_or_else(provider_failed)?;
    let body = ctx
        .http
        .post(format!("{}?charset=utf-8", alipay.gateway()))
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded;charset=utf-8",
        )
        .timeout(TIMEOUT)
        .body(encode(&params))
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| {
            warn!(
                subscription_id = out_trade_no,
                "Alipay refused the order query: {e}"
            );
            provider_failed()
        })?
        .bytes()
        .await
        .map_err(|_| provider_failed())?;
    let answer: Value = serde_json::from_slice(&body).map_err(|_| provider_failed())?;
    read_trade(&answer["alipay_trade_query_response"], Utc::now()).map_err(|code| {
        warn!(
            subscription_id = out_trade_no,
            code, "Alipay answered the order query with an error"
        );
        provider_failed()
    })
}

/// The order in a query response, or the error code it carries.
fn read_trade(r: &Value, now: DateTime<Utc>) -> Result<Option<Trade>, String> {
    let s = |k: &str| r[k].as_str().unwrap_or_default().to_string();
    match (s("code").as_str(), s("sub_code").as_str()) {
        ("10000", _) => Ok(Some(Trade {
            status: s("trade_status"),
            amount: s("total_amount"),
            paid_at: alipay_time(&s("send_pay_date")).unwrap_or(now),
        })),
        ("40004", "ACQ.TRADE_NOT_EXIST") => Ok(None),
        (code, sub) => Err(format!("{code} {sub}")),
    }
}

/// API-084: the buyer is back from paying; record the order if Alipay says
/// it is paid, asking as `app_id`, the app the caller buys from. What it
/// grants access through, or `None` while unpaid.
pub async fn check(
    st: &St,
    alipay: &Alipay,
    app_id: &str,
    account: i32,
    out_trade_no: &str,
) -> ApiResult<Option<DateTime<Utc>>> {
    if let Err(wait) = st.limiter.hit(
        &format!("alipay-check:{account}"),
        30,
        Duration::from_secs(3600),
    ) {
        return Err(rate_limited(
            "alipay-check",
            Some(account),
            "Too many payment checks in the last hour. Try again later.",
            wait,
        ));
    }
    if account_of(out_trade_no) != Some(account) {
        return Err(err(ErrorCode::NotFound, "No such order."));
    }
    let recorded: Option<DateTime<Utc>> =
        sqlx::query_scalar("SELECT paid_until FROM subscriptions WHERE id = $1")
            .bind(out_trade_no)
            .fetch_optional(&st.db)
            .await?;
    if recorded.is_some() {
        return Ok(recorded);
    }
    let Some(trade) = query(&st.pay(), alipay, app_id, out_trade_no)
        .await?
        .filter(|t| paid(&t.status))
    else {
        return Ok(None);
    };
    let pack = pack_costing(&trade.amount).ok_or_else(|| {
        warn!(subscription_id = out_trade_no, amount = %trade.amount, "a paid order for neither pack");
        provider_failed()
    })?;
    let mut tx = st.db.begin().await?;
    let r = record(
        &mut tx,
        &st.cfg,
        alipay,
        account,
        out_trade_no,
        pack,
        trade.paid_at,
    )
    .await?;
    tx.commit().await?;
    let r = r.ok_or_else(|| err(ErrorCode::NotFound, "No such order."))?;
    if r.new {
        warn_if_duplicate(st, out_trade_no).await;
    }
    Ok(Some(r.until))
}

// ---- notifications ----

/// What a notification did, for the log.
pub struct Notice {
    pub notify_id: String,
    pub kind: String,
    pub out_trade_no: String,
    pub outcome: Outcome,
}

fn unauthorized() -> ApiError {
    err(ErrorCode::Unauthorized, "Bad signature.")
}

/// API-083: verify one notification, then store it and record the pack in
/// one transaction (INV-033).
pub async fn notify(st: &St, alipay: &Alipay, body: &[u8]) -> ApiResult<Notice> {
    let params: BTreeMap<String, String> = form_urlencoded::parse(body).into_owned().collect();
    if !verified(&alipay.alipay_key, &params) {
        return Err(unauthorized());
    }
    let get = |k: &str| params.get(k).map(String::as_str).unwrap_or_default();
    let (notify_id, status, out_trade_no) =
        (get("notify_id"), get("trade_status"), get("out_trade_no"));
    let kind = format!("alipay.{status}");
    let payload: BTreeMap<&String, &String> = params.iter().filter(|(k, _)| *k != "sign").collect();

    let mut tx = st.db.begin().await?;
    let inserted = sqlx::query(
        "INSERT INTO billing_events (id, type, subscription_id, payload) VALUES ($1, $2, $3, $4)
         ON CONFLICT (id) DO NOTHING",
    )
    .bind(format!("alipay:{notify_id}"))
    .bind(&kind)
    .bind(Some(out_trade_no).filter(|o| !o.is_empty()))
    .bind(sqlx::types::Json(&payload))
    .execute(&mut *tx)
    .await?;
    let outcome = if inserted.rows_affected() == 0 {
        Outcome::Duplicate
    } else if !alipay.ours(get("app_id")) {
        Outcome::WrongMode
    } else if !paid(status) {
        Outcome::Stored
    } else {
        match (account_of(out_trade_no), pack_costing(get("total_amount"))) {
            (None, _) => Outcome::UnknownAccount,
            (_, None) => Outcome::UnknownProduct,
            (Some(account), Some(pack)) => {
                let paid_at = alipay_time(get("gmt_payment")).unwrap_or_else(Utc::now);
                match record(
                    &mut tx,
                    &st.cfg,
                    alipay,
                    account,
                    out_trade_no,
                    pack,
                    paid_at,
                )
                .await?
                {
                    None => Outcome::UnknownAccount,
                    Some(r) if r.new => Outcome::Applied,
                    Some(_) => Outcome::Duplicate,
                }
            }
        }
    };
    tx.commit().await?;
    if outcome == Outcome::Applied {
        warn_if_duplicate(st, out_trade_no).await;
    }
    Ok(Notice {
        notify_id: notify_id.to_string(),
        kind,
        out_trade_no: out_trade_no.to_string(),
        outcome,
    })
}

async fn warn_if_duplicate(st: &St, id: &str) {
    if let Err(e) = billing::warn_if_duplicate(st, id).await {
        warn!(error = %e, "duplicate subscription check failed");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    #[test]
    fn packs_by_plan_and_price() {
        assert_eq!(pack("monthly").unwrap().amount, "15.00");
        assert_eq!(pack("yearly").unwrap().amount, "150.00");
        assert!(pack("weekly").is_none());
        assert_eq!(pack_costing("15.00").unwrap().plan, "monthly");
        assert_eq!(pack_costing("150.00").unwrap().plan, "yearly");
        // Only the exact price buys a pack.
        assert!(pack_costing("15").is_none());
        assert!(pack_costing("0.01").is_none());
    }

    #[test]
    fn order_numbers() {
        assert_eq!(account_of("ALIPAY_12345678_1793000000123"), Some(12345678));
        assert_eq!(account_of("ALIPAY_7_1"), Some(7));
        assert_eq!(account_of("ALIPAY_7"), None);
        assert_eq!(account_of("ALIPAY_x_1"), None);
        assert_eq!(account_of("ALIPAY_7_x"), None);
        assert_eq!(account_of("ORD_7_1"), None);
    }

    #[test]
    fn calendar_months_from_payment_or_trial_end() {
        let monthly = pack("monthly").unwrap();
        let yearly = pack("yearly").unwrap();
        // 2026-11-04 12:00 in Beijing.
        let paid = t("2026-11-04T04:00:00Z");
        assert_eq!(paid_until(monthly, paid, None), t("2026-12-04T04:00:00Z"));
        assert_eq!(paid_until(yearly, paid, None), t("2027-11-04T04:00:00Z"));
        // Bought inside the trial: it starts when the trial ends.
        let trial = t("2026-11-20T00:00:00Z");
        assert_eq!(
            paid_until(monthly, paid, Some(trial)),
            t("2026-12-20T00:00:00Z")
        );
        // A trial already over changes nothing.
        let over = t("2026-10-01T00:00:00Z");
        assert_eq!(
            paid_until(monthly, paid, Some(over)),
            t("2026-12-04T04:00:00Z")
        );
        // The month is counted in Beijing: 31 January there runs to 28 February.
        let jan31 = t("2027-01-30T17:00:00Z");
        assert_eq!(paid_until(monthly, jan31, None), t("2027-02-27T17:00:00Z"));
    }

    #[test]
    fn times_are_beijing_time() {
        assert_eq!(
            alipay_time("2026-11-04 12:00:00"),
            Some(t("2026-11-04T04:00:00Z"))
        );
        assert_eq!(alipay_time(""), None);
        assert_eq!(alipay_time("2026-11-04T12:00:00Z"), None);
    }

    #[test]
    fn signing_strings() {
        let params: BTreeMap<String, String> = [
            ("sign", "S"),
            ("sign_type", "RSA2"),
            ("b", "2 x"),
            ("a", "1"),
            ("empty", ""),
            ("c", "{\"k\":\"v\"}"),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        // A request signs sign_type too; a notification doesn't. Empty values
        // drop out, and values stay as they are.
        assert_eq!(
            signing_string(&params, &["sign"]),
            "a=1&b=2 x&c={\"k\":\"v\"}&sign_type=RSA2"
        );
        assert_eq!(
            signing_string(&params, &["sign", "sign_type"]),
            "a=1&b=2 x&c={\"k\":\"v\"}"
        );
    }

    #[test]
    fn calls_carry_the_common_parameters() {
        let p = call(
            "2021007115697060",
            "alipay.trade.query",
            &json!({"out_trade_no": "ALIPAY_7_1"}),
            t("2026-11-04T04:00:00Z"),
        );
        assert_eq!(p["timestamp"], "2026-11-04 12:00:00");
        assert_eq!(p["biz_content"], r#"{"out_trade_no":"ALIPAY_7_1"}"#);
        assert_eq!(
            encode(&p),
            "app_id=2021007115697060&biz_content=%7B%22out_trade_no%22%3A%22ALIPAY_7_1%22%7D\
             &charset=utf-8&format=JSON&method=alipay.trade.query&sign_type=RSA2\
             &timestamp=2026-11-04+12%3A00%3A00&version=1.0"
        );
    }

    #[test]
    fn query_answers() {
        let now = t("2026-11-04T05:00:00Z");
        let success = json!({
            "code": "10000", "msg": "Success", "trade_status": "TRADE_SUCCESS",
            "total_amount": "150.00", "send_pay_date": "2026-11-04 12:00:00",
        });
        let trade = read_trade(&success, now).unwrap().unwrap();
        assert!(paid(&trade.status));
        assert_eq!(trade.amount, "150.00");
        assert_eq!(trade.paid_at, t("2026-11-04T04:00:00Z"));
        let waiting =
            json!({"code": "10000", "trade_status": "WAIT_BUYER_PAY", "total_amount": "15.00"});
        let trade = read_trade(&waiting, now).unwrap().unwrap();
        assert!(!paid(&trade.status));
        assert_eq!(trade.paid_at, now);
        let missing = json!({"code": "40004", "sub_code": "ACQ.TRADE_NOT_EXIST"});
        assert!(read_trade(&missing, now).unwrap().is_none());
        // Anything else is Alipay failing.
        let busy = json!({"code": "20000", "sub_code": "isp.unknow-error"});
        assert_eq!(
            read_trade(&busy, now).err().as_deref(),
            Some("20000 isp.unknow-error")
        );
        assert!(read_trade(&Value::Null, now).is_err());
    }
}
