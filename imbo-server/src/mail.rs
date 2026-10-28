//! Sending login codes over SMTP, or to the log in development.

use std::time::Duration;

use anyhow::Context;
use lettre::message::{Attachment, Mailbox, MultiPart, SinglePart, header::ContentType};
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
        let msg = code_message(self.from.clone(), to, code)?;
        smtp.send(msg).await.context("smtp send")?;
        info!(email, "login code sent");
        Ok(())
    }

    /// API-061: tell `IMBO_REPORT_EMAIL` a report is waiting, with only the
    /// admin page link: no names, no messages (ADR-039).
    pub async fn send_report(&self, cfg: &Config, report: i64) -> anyhow::Result<()> {
        let addr = cfg
            .report_email
            .as_deref()
            .context("IMBO_REPORT_EMAIL is not set")?;
        let to: Mailbox = addr.parse().context("IMBO_REPORT_EMAIL")?;
        let link = report_link(cfg, report);
        let Some(smtp) = &self.smtp else {
            info!("mail-log-only report={report} to={addr} link={link}");
            return Ok(());
        };
        smtp.send(report_message(self.from.clone(), to, &link)?)
            .await
            .context("smtp send")?;
        info!(report_id = report, "report mail sent");
        Ok(())
    }
}

/// The admin page in the web app, opened on this report (PAGE-WEB-ADMIN).
fn report_link(cfg: &Config, report: i64) -> String {
    let web = cfg.web_origin.as_deref().unwrap_or("https://app.imbo.dev");
    format!("{web}/admin?report={report}")
}

fn report_message(from: Mailbox, to: Mailbox, link: &str) -> anyhow::Result<Message> {
    Ok(Message::builder()
        .from(from)
        .to(to)
        .subject("New Imbo report")
        .singlepart(SinglePart::plain(format!(
            "A new report is waiting. Handle it within 24 hours:\n\n{link}\n"
        )))?)
}

const CODE_HTML: &str = include_str!("mail_code.html");
const WORDMARK: &[u8] = include_bytes!("../assets/wordmark-compact-dark.png");

/// The sign-in code email (DESIGN API-001): a plain-text part, and an HTML
/// part with the wordmark attached inline.
fn code_message(from: Mailbox, to: Mailbox, code: &str) -> anyhow::Result<Message> {
    let text = format!(
        "Your Imbo sign-in code: {code}\n\n\
         Enter it where you asked to sign in. It expires in 10 minutes.\n\n\
         If you didn't try to sign in to Imbo, ignore this email; nobody can sign in without the code.\n\n\
         --\n\
         Imbo: instant messaging for AI coding agents\n\
         https://imbo.dev\n"
    );
    let html = CODE_HTML.replace("{{code}}", code);
    let logo = Attachment::new_inline("wordmark".into())
        .body(WORDMARK.to_vec(), ContentType::parse("image/png")?);
    Ok(Message::builder()
        .from(from)
        .to(to)
        .subject(format!("Imbo code: {code}"))
        .multipart(
            MultiPart::alternative()
                .singlepart(SinglePart::plain(text))
                .multipart(
                    MultiPart::related()
                        .singlepart(SinglePart::html(html))
                        .singlepart(logo),
                ),
        )?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_email_has_only_the_link() {
        let cfg = Config {
            web_origin: Some("https://app.imbo.dev".into()),
            ..Config::default()
        };
        let msg = report_message(
            "Imbo <login@imbo.dev>".parse().unwrap(),
            "support@imbo.test".parse().unwrap(),
            &report_link(&cfg, 42),
        )
        .unwrap();
        let raw = String::from_utf8(msg.formatted()).unwrap();
        assert!(raw.contains("Subject: New Imbo report"));
        assert!(raw.contains("https://app.imbo.dev/admin?report=42"));
    }

    #[test]
    fn code_email_has_text_html_and_logo() {
        let msg = code_message(
            "Imbo <login@imbo.dev>".parse().unwrap(),
            "a@example.com".parse().unwrap(),
            "123456",
        )
        .unwrap();
        let raw = String::from_utf8(msg.formatted()).unwrap();
        assert!(raw.contains("Subject: Imbo code: 123456"));
        assert!(raw.contains("multipart/alternative"));
        assert!(raw.contains("multipart/related"));
        assert!(raw.contains("Your Imbo sign-in code: 123456"));
        assert!(raw.contains("Content-ID: <wordmark>"));
        let html = CODE_HTML.replace("{{code}}", "123456");
        assert!(html.contains(">123456</td>"));
        assert!(!html.contains("{{code}}"));
    }
}
