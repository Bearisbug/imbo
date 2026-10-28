//! Server configuration, read from environment variables.

use std::path::PathBuf;

use anyhow::{Context, bail};
use chrono::{DateTime, NaiveDate, Utc};

#[derive(Clone, Debug, Default)]
pub struct Config {
    /// Public host name. With TLS on, certificates are fetched for it.
    pub domain: Option<String>,
    pub tls: bool,
    pub listen: String,
    pub database_url: Option<String>,
    pub data_dir: PathBuf,
    pub smtp_url: Option<String>,
    pub mail_from: String,
    pub mail_log_only: bool,
    /// Lower-case email domains allowed to sign up; empty = everyone.
    pub signup_domains: Vec<String>,
    pub acme_email: Option<String>,
    pub acme_staging: bool,
    pub dev: bool,
    /// File whose mtime is the last successful backup (for /v1/health).
    pub backup_stamp: Option<PathBuf>,
    pub min_client_version: String,
    /// Payments; `None` means nobody is charged and the web app hides billing (ADR-021).
    pub creem: Option<Creem>,
    /// First day of charging; unset means everyone is free (RULE-007).
    pub billing_start: Option<DateTime<Utc>>,
    /// The web app's origin, the only one allowed cross-origin with cookies (ADR-023).
    pub web_origin: Option<String>,
    /// Sent with every Expo push request when the project requires it (ADR-034).
    pub expo_access_token: Option<String>,
    /// Log the notifications instead of calling Expo, for local tests.
    pub push_log_only: bool,
    /// Lower-case emails whose web sign-in is ROLE-Admin (ADR-040).
    pub admin_emails: Vec<String>,
    /// Where new reports are announced; unset means only the log (API-061).
    pub report_email: Option<String>,
    /// The store reviewers' address and its fixed code (API-001).
    pub review: Option<Review>,
}

/// Codes asked for `email` are `code` and no mail is sent (API-001).
#[derive(Clone)]
pub struct Review {
    pub email: String,
    pub code: String,
}

impl std::fmt::Debug for Review {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Review({})", self.email)
    }
}

#[derive(Clone)]
pub struct Creem {
    pub api_key: String,
    pub webhook_secret: String,
    pub product_monthly: String,
    pub product_yearly: String,
}

impl std::fmt::Debug for Creem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Creem({} mode)", self.mode())
    }
}

impl Creem {
    /// `test` for `creem_test_` keys, otherwise `live` (ADR-021).
    pub fn mode(&self) -> &'static str {
        if self.api_key.starts_with("creem_test_") {
            "test"
        } else {
            "live"
        }
    }

    pub fn api_base(&self) -> &'static str {
        if self.mode() == "test" {
            "https://test-api.creem.io"
        } else {
            "https://api.creem.io"
        }
    }

    pub fn plan_of(&self, product: &str) -> Option<&'static str> {
        if product == self.product_monthly {
            Some("monthly")
        } else if product == self.product_yearly {
            Some("yearly")
        } else {
            None
        }
    }
}

fn creem_from_env() -> anyhow::Result<Option<Creem>> {
    let names = [
        "IMBO_CREEM_API_KEY",
        "IMBO_CREEM_WEBHOOK_SECRET",
        "IMBO_CREEM_PRODUCT_MONTHLY",
        "IMBO_CREEM_PRODUCT_YEARLY",
    ];
    let vals: Vec<Option<String>> = names.iter().map(|n| var(n)).collect();
    if vals.iter().all(Option::is_none) {
        return Ok(None);
    }
    if let Some(i) = vals.iter().position(Option::is_none) {
        bail!(
            "{} is not set; set all of {} or none",
            names[i],
            names.join(", ")
        );
    }
    let mut v = vals.into_iter().map(Option::unwrap_or_default);
    Ok(Some(Creem {
        api_key: v.next().unwrap_or_default(),
        webhook_secret: v.next().unwrap_or_default(),
        product_monthly: v.next().unwrap_or_default(),
        product_yearly: v.next().unwrap_or_default(),
    }))
}

/// Both IMBO_REVIEW_EMAIL and IMBO_REVIEW_CODE, or no review sign-in.
fn review_from_env() -> anyhow::Result<Option<Review>> {
    let (Some(email), Some(code)) = (var("IMBO_REVIEW_EMAIL"), var("IMBO_REVIEW_CODE")) else {
        return Ok(None);
    };
    if code.len() != 6 || !code.bytes().all(|b| b.is_ascii_digit()) {
        bail!("IMBO_REVIEW_CODE must be 6 digits");
    }
    Ok(Some(Review {
        email: email.to_lowercase(),
        code,
    }))
}

/// `2026-11-01` (midnight UTC) or an RFC 3339 time.
fn billing_start_from_env() -> anyhow::Result<Option<DateTime<Utc>>> {
    let Some(raw) = var("IMBO_BILLING_START") else {
        return Ok(None);
    };
    if let Ok(t) = DateTime::parse_from_rfc3339(&raw) {
        return Ok(Some(t.with_timezone(&Utc)));
    }
    let day = NaiveDate::parse_from_str(&raw, "%Y-%m-%d")
        .with_context(|| format!("IMBO_BILLING_START={raw}: use 2026-11-01 or an RFC 3339 time"))?;
    Ok(day.and_hms_opt(0, 0, 0).map(|t| t.and_utc()))
}

fn var(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

fn flag(name: &str) -> bool {
    matches!(var(name).as_deref(), Some("1" | "true" | "yes" | "on"))
}

impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        let domain = var("IMBO_DOMAIN");
        let tls =
            domain.is_some() && !matches!(var("IMBO_TLS").as_deref(), Some("off" | "0" | "false"));
        let listen = var("IMBO_LISTEN").unwrap_or_else(|| {
            if tls {
                "0.0.0.0:443".into()
            } else {
                "0.0.0.0:8080".into()
            }
        });
        let data_dir = var("IMBO_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("imbo-data"));
        let mail_log_only = flag("IMBO_MAIL_LOG_ONLY");
        let smtp_url = var("IMBO_SMTP_URL");
        if smtp_url.is_none() && !mail_log_only {
            bail!(
                "set IMBO_SMTP_URL (e.g. smtps://user:pass@smtp.example.com:465), or IMBO_MAIL_LOG_ONLY=1 to print codes to the log"
            );
        }
        let mail_from = var("IMBO_MAIL_FROM").unwrap_or_else(|| match &domain {
            Some(d) => format!("Imbo <login@{}>", d.trim_start_matches("api.")),
            None => "Imbo <login@localhost>".into(),
        });
        let signup_domains = var("IMBO_SIGNUP_EMAIL_DOMAINS")
            .map(|v| {
                v.split(',')
                    .map(|d| d.trim().trim_start_matches('@').to_lowercase())
                    .filter(|d| !d.is_empty())
                    .collect()
            })
            .unwrap_or_default();
        Ok(Self {
            domain,
            tls,
            listen,
            database_url: var("DATABASE_URL"),
            data_dir,
            smtp_url,
            mail_from,
            mail_log_only,
            signup_domains,
            acme_email: var("IMBO_ACME_EMAIL"),
            acme_staging: flag("IMBO_ACME_STAGING"),
            dev: flag("IMBO_DEV"),
            backup_stamp: var("IMBO_BACKUP_STAMP").map(PathBuf::from),
            min_client_version: var("IMBO_MIN_CLIENT_VERSION").unwrap_or_else(|| "0.1.0".into()),
            creem: creem_from_env()?,
            billing_start: billing_start_from_env()?,
            web_origin: var("IMBO_WEB_ORIGIN").map(|o| o.trim_end_matches('/').to_string()),
            expo_access_token: var("IMBO_EXPO_ACCESS_TOKEN"),
            push_log_only: flag("IMBO_PUSH_LOG_ONLY"),
            admin_emails: var("IMBO_ADMIN_EMAILS")
                .map(|v| {
                    v.split(',')
                        .map(|e| e.trim().to_lowercase())
                        .filter(|e| !e.is_empty())
                        .collect()
                })
                .unwrap_or_default(),
            report_email: var("IMBO_REPORT_EMAIL"),
            review: review_from_env()?,
        })
    }

    pub fn is_admin_email(&self, email: &str) -> bool {
        let email = email.to_lowercase();
        self.admin_emails.contains(&email)
    }

    /// API-001: the fixed code when `email` is the store reviewers' address.
    pub fn review_code(&self, email: &str) -> Option<&str> {
        let r = self.review.as_ref()?;
        (r.email == email.to_lowercase()).then_some(r.code.as_str())
    }

    /// RULE-007: charging needs both Creem and a start day.
    pub fn billing_on(&self) -> bool {
        self.creem.is_some() && self.billing_start.is_some()
    }

    pub fn signup_allowed(&self, email: &str) -> bool {
        if self.signup_domains.is_empty() {
            return true;
        }
        let domain = email
            .rsplit_once('@')
            .map(|(_, d)| d.to_lowercase())
            .unwrap_or_default();
        self.signup_domains.contains(&domain)
    }
}
