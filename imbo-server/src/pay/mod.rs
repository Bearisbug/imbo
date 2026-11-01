//! The payment-channel port (DESIGN §6). The core reaches a channel only
//! through `Channel`, picked by a subscription's `provider` or by the shape
//! of its ID, never by name; Waffo, the App Store, Alipay time packs and
//! gifts are its adapters. What every channel needs alike — the 409 on a
//! second purchase, purchase limits, when to ask a channel, stopping renewals
//! on deletion and bans — is done here once. A channel's own endpoints
//! (API-035, API-071, API-072) are bound to its adapter in the router.

pub mod alipay;
pub mod apple;
pub mod waffo;

use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use imbo_proto::ErrorCode;
use sqlx::PgPool;
use tracing::warn;

use crate::billing::{self, SubRow};
use crate::config::Config;
use crate::error::{ApiError, ApiResult, err, rate_limited};
use crate::state::St;

/// A call to a channel gives up after this, unless the channel sets less.
const TIMEOUT: Duration = Duration::from_secs(10);
/// The account page asks the channels at most this often per account.
const PAGE_EVERY: Duration = Duration::from_secs(5);
/// Before refusing a send, at most this often per account.
const SEND_EVERY: Duration = Duration::from_secs(60);

pub fn http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(TIMEOUT)
        .build()
        .expect("build HTTP client")
}

/// A channel failed or did not answer in time.
pub fn provider_failed() -> ApiError {
    err(
        ErrorCode::PaymentProviderFailed,
        "The payment service is not answering. Try again shortly.",
    )
}

pub fn already_subscribed() -> ApiError {
    err(
        ErrorCode::AlreadySubscribed,
        "You already have a subscription or a time pack.",
    )
}

/// What a channel works with, in the server or in a billing command.
pub struct Ctx<'a> {
    pub db: &'a PgPool,
    pub cfg: &'a Config,
    pub http: &'a reqwest::Client,
}

/// What starting a purchase gave.
pub enum Begun {
    /// Send the buyer to this page. It signs them in, so it is never logged.
    Pay(String),
    /// Paid already, through this date.
    Granted(DateTime<Utc>),
}

/// Who can stop a subscription renewing.
#[derive(Clone, Copy, PartialEq)]
pub enum Stopper {
    /// The server, by `Channel::stop`.
    Server,
    /// Only its owner, at the channel.
    Owner,
    /// Nobody: it doesn't renew.
    Nobody,
}

/// What a webhook or notification did, for the log.
#[derive(Debug, PartialEq)]
pub enum Outcome {
    Applied,
    Duplicate,
    /// Older than what is recorded.
    Stale,
    UnknownAccount,
    WrongMode,
    /// A billing period other than the two plans.
    UnknownProduct,
    /// A type that doesn't change subscriptions.
    Stored,
}

impl Outcome {
    /// The outcome names of `EVT:billing_event` (DESIGN §20).
    pub fn as_str(&self) -> &'static str {
        match self {
            Outcome::Applied => "applied",
            Outcome::Duplicate => "duplicate",
            Outcome::Stale => "stale",
            Outcome::UnknownAccount | Outcome::UnknownProduct => "unmapped",
            Outcome::WrongMode | Outcome::Stored => "ignored",
        }
    }
}

/// One payment channel. The defaults are what a channel without the
/// capability does; each adapter writes the subscriptions it learns of
/// itself, never over a newer record.
#[async_trait]
pub trait Channel: Send + Sync {
    /// Its `subscriptions.provider`.
    fn name(&self) -> &'static str;

    /// Whether a subscription ID has this channel's shape.
    fn owns(&self, id: &str) -> bool;

    /// How the operator's messages name it.
    fn title(&self) -> &'static str {
        self.name()
    }

    /// Paid for by the account: it stands in the way of buying again and
    /// counts as paid. A gift doesn't (ADR-060).
    fn voluntary(&self) -> bool {
        true
    }

    fn stopper(&self) -> Stopper {
        Stopper::Nobody
    }

    /// Where to buy; `plan` is Waffo's, a time pack is picked on its page.
    async fn begin(&self, _ctx: &Ctx<'_>, _account: i32, _plan: &str) -> ApiResult<Begun> {
        Err(err(ErrorCode::NotFound, "Nothing is sold here."))
    }

    /// The account's page for managing what it bought there, if there is one.
    async fn portal(&self, _ctx: &Ctx<'_>, _account: i32) -> ApiResult<Option<String>> {
        Ok(None)
    }

    /// Ask the channel what the account has now and record it.
    async fn refresh(&self, _ctx: &Ctx<'_>, _account: i32) -> ApiResult<()> {
        Ok(())
    }

    /// `imbo-server billing pull`: record what the channel has under this
    /// ID, for a notification that never arrived; a line for the operator.
    async fn pull(&self, _ctx: &Ctx<'_>, id: &str) -> anyhow::Result<String> {
        anyhow::bail!("{id}: {} subscriptions are not pulled", self.name())
    }

    /// For `Stopper::Owner`, before deleting the account: whether the
    /// channel will charge again. Records what it says.
    async fn renews(&self, _ctx: &Ctx<'_>, _sub: &SubRow) -> ApiResult<bool> {
        Ok(true)
    }

    /// For `Stopper::Server`: stop renewing, without a refund.
    async fn stop(&self, _ctx: &Ctx<'_>, _sub: &SubRow) -> ApiResult<()> {
        Ok(())
    }
}

/// ADR-060: ops writes it; it grants access without being paid for.
struct Gift;

#[async_trait]
impl Channel for Gift {
    fn name(&self) -> &'static str {
        "gift"
    }

    fn owns(&self, id: &str) -> bool {
        id.starts_with("gift-")
    }

    fn voluntary(&self) -> bool {
        false
    }
}

/// The channels this server takes, in the order an ID is offered to them.
pub fn channels(cfg: &Config) -> Vec<Box<dyn Channel + '_>> {
    let mut all: Vec<Box<dyn Channel + '_>> = vec![];
    if let Some(w) = &cfg.waffo {
        all.push(Box::new(waffo::Adapter(w)));
    }
    if let Some(a) = &cfg.apple {
        all.push(Box::new(apple::Adapter(a)));
    }
    if let Some(w) = &cfg.watcha {
        all.push(Box::new(alipay::Adapter(w)));
    }
    all.push(Box::new(Gift));
    all
}

fn by_name<'a>(cfg: &'a Config, provider: &str) -> Option<Box<dyn Channel + 'a>> {
    channels(cfg).into_iter().find(|c| c.name() == provider)
}

/// The channel a subscription ID belongs to.
pub fn owner<'a>(cfg: &'a Config, id: &str) -> Option<Box<dyn Channel + 'a>> {
    channels(cfg).into_iter().find(|c| c.owns(id))
}

/// ADR-060: paid for by the account. One from a channel this server no
/// longer takes was paid for too.
pub fn voluntary(cfg: &Config, sub: &SubRow) -> bool {
    by_name(cfg, &sub.provider).is_none_or(|c| c.voluntary())
}

/// A subscription the account paid for that grants access at `now`.
pub fn paid_grant(cfg: &Config, sub: &SubRow, now: DateTime<Utc>) -> bool {
    voluntary(cfg, sub) && sub.grants_until().is_some_and(|g| g > now)
}

/// API-033, API-080: start buying from `ch`, at most `per_hour` times an
/// hour under `scope`. Refused while a subscription the account paid for
/// grants access.
pub async fn begin(
    st: &St,
    ch: &dyn Channel,
    account: i32,
    plan: &str,
    (scope, per_hour): (&str, usize),
) -> ApiResult<Begun> {
    if let Err(wait) = st.limiter.hit(
        &format!("{scope}:{account}"),
        per_hour,
        Duration::from_secs(3600),
    ) {
        return Err(rate_limited(
            scope,
            Some(account),
            "Too many purchase attempts in the last hour. Try again later.",
            wait,
        ));
    }
    let now = Utc::now();
    let subs = billing::subscriptions(&st.db, &st.cfg, &[account]).await?;
    if subs.iter().any(|s| paid_grant(&st.cfg, s, now)) {
        return Err(already_subscribed());
    }
    ch.begin(&st.pay(), account, plan).await
}

/// API-034: the page of the first of the account's subscriptions whose
/// channel has one.
pub async fn portal(st: &St, account: i32) -> ApiResult<String> {
    let ctx = st.pay();
    for s in billing::subscriptions(&st.db, &st.cfg, &[account]).await? {
        if let Some(ch) = by_name(&st.cfg, &s.provider)
            && let Some(url) = ch.portal(&ctx, account).await?
        {
            return Ok(url);
        }
    }
    Err(err(
        ErrorCode::NotFound,
        "You have no subscription to manage here. App Store subscriptions are managed in Settings on your iPhone.",
    ))
}

/// What a refresh comes before.
#[derive(Clone, Copy)]
pub enum When {
    /// Answering the account page (API-032).
    Page,
    /// Refusing a send (RULE-008).
    Send,
}

/// Ask every channel what the account has now and record it. Skipped when
/// billing is off, while a subscription it paid for grants access, or when
/// it asked moments ago; a channel that fails only logs, and the caller
/// answers from what is recorded.
pub async fn refresh(st: &St, account: i32, when: When) {
    if !st.cfg.billing_on() {
        return;
    }
    let (key, every) = match when {
        When::Page => ("refresh-page", PAGE_EVERY),
        When::Send => ("refresh-send", SEND_EVERY),
    };
    if st
        .limiter
        .hit(&format!("{key}:{account}"), 1, every)
        .is_err()
    {
        return;
    }
    let now = Utc::now();
    match billing::subscriptions(&st.db, &st.cfg, &[account]).await {
        Ok(subs) if subs.iter().any(|s| paid_grant(&st.cfg, s, now)) => return,
        Ok(_) => {}
        Err(e) => {
            warn!(account_id = account, "cannot read subscriptions: {e}");
            return;
        }
    }
    let ctx = st.pay();
    for ch in channels(&st.cfg) {
        if let Err(e) = ch.refresh(&ctx, account).await {
            warn!(
                account_id = account,
                channel = ch.name(),
                error = %e.0.detail,
                "refresh failed"
            );
        }
    }
}

/// The account's subscriptions that renew as recorded, with their channels.
async fn renewing<'a>(
    ctx: &Ctx<'a>,
    account: i32,
) -> Result<Vec<(SubRow, Box<dyn Channel + 'a>)>, sqlx::Error> {
    Ok(billing::subscriptions(ctx.db, ctx.cfg, &[account])
        .await?
        .into_iter()
        .filter(|s| matches!(s.status.as_str(), "active" | "past_due"))
        .filter_map(|s| by_name(ctx.cfg, &s.provider).map(|c| (s, c)))
        .collect())
}

/// Only its owner can stop it, and it isn't refunded, which stops it too.
fn owner_stops(s: &SubRow, ch: &dyn Channel) -> bool {
    ch.stopper() == Stopper::Owner && s.revoked_at.is_none()
}

/// API-052: a subscription only its owner can stop, still renewing at its
/// channel, stands in the way of deleting the account. 502 when the channel
/// doesn't answer.
pub async fn before_delete(ctx: &Ctx<'_>, account: i32) -> ApiResult<()> {
    for (s, ch) in renewing(ctx, account).await? {
        if owner_stops(&s, ch.as_ref()) && ch.renews(ctx, &s).await? {
            return Err(err(
                ErrorCode::AppStoreRenewing,
                "Cancel your App Store subscription in Settings on your iPhone first; deleting doesn't stop Apple charging you.",
            ));
        }
    }
    Ok(())
}

/// API-052, API-066, API-070: stop every renewing subscription the server
/// can stop; the first failure ends it.
pub async fn stop_renewing(ctx: &Ctx<'_>, account: i32) -> ApiResult<()> {
    for (s, ch) in renewing(ctx, account).await? {
        if ch.stopper() == Stopper::Server {
            ch.stop(ctx, &s).await?;
        }
    }
    Ok(())
}

/// API-066: whether the account has a renewing subscription only it can stop.
pub async fn owner_must_stop(ctx: &Ctx<'_>, account: i32) -> Result<bool, sqlx::Error> {
    Ok(renewing(ctx, account)
        .await?
        .iter()
        .any(|(s, ch)| owner_stops(s, ch.as_ref())))
}
