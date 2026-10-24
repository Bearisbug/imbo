//! Sending login codes over SMTP, or to the log in development.

use std::time::Duration;

use anyhow::Context;
use lettre::message::{Mailbox, header::ContentType};
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use tracing::info;

use crate::config::Config;

pub struct Mailer {
    smtp: Option<AsyncSmtpTransport<Tokio1Executor>>,
    from: Mailbox,
}

impl Mailer {
    pub fn new(cfg: &Config) -> anyhow::Result<Self> {
        let from: Mailbox = cfg.mail_from.parse().context("IMBO_MAIL_FROM")?;
        let smtp = if cfg.mail_log_only {
            None
        } else {
            let url = cfg.smtp_url.as_deref().context("IMBO_SMTP_URL")?;
            Some(
                AsyncSmtpTransport::<Tokio1Executor>::from_url(url)
                    .context("IMBO_SMTP_URL")?
                    .timeout(Some(Duration::from_secs(10)))
                    .build(),
            )
        };
        Ok(Self { smtp, from })
    }

    pub async fn send_code(&self, email: &str, code: &str) -> anyhow::Result<()> {
        let Some(smtp) = &self.smtp else {
            info!("mail-log-only code={code} email={email}");
            return Ok(());
        };
        let to: Mailbox = email.parse().context("recipient")?;
        let body = format!(
            "Your Imbo sign-in code is {code}\n\
             你的 Imbo 登录验证码是 {code}\n\n\
             It expires in 10 minutes. If you did not try to sign in, ignore this email.\n\
             10 分钟内有效。如果不是你本人在登录，忽略这封邮件即可。\n"
        );
        let msg = Message::builder()
            .from(self.from.clone())
            .to(to)
            .subject(format!("Imbo code: {code}"))
            .header(ContentType::TEXT_PLAIN)
            .body(body)?;
        smtp.send(msg).await.context("smtp send")?;
        info!(email, "login code sent");
        Ok(())
    }
}
