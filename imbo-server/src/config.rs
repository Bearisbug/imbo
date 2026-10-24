//! Server configuration, read from environment variables.

use std::path::PathBuf;

use anyhow::bail;

#[derive(Clone, Debug)]
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
            min_client_version: "0.1.0".into(),
        })
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
