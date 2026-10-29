//! Server configuration, read from environment variables.

use std::path::PathBuf;

use anyhow::{Context, bail};
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use chrono::{DateTime, NaiveDate, Utc};
use ring::rand::SystemRandom;
use ring::signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, RsaKeyPair};

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
    pub waffo: Option<Waffo>,
    /// App Store subscriptions; only with Waffo, whose mode they follow (ADR-043).
    pub apple: Option<Apple>,
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

/// Waffo, the payment platform (ADR-042).
#[derive(Clone)]
pub struct Waffo {
    pub merchant_id: String,
    pub store_id: String,
    /// The API private key (PKCS#8 or PKCS#1 DER), checked at startup. A key
    /// belongs to the environment it was made in.
    pub key: Vec<u8>,
    /// `IMBO_WAFFO_ENV=prod`: real cards. `test`: test cards only (ADR-021).
    pub live: bool,
    pub product_monthly: String,
    pub product_yearly: String,
    /// The RSAPublicKey (DER) that webhooks are verified with (API-035).
    pub webhook_key: Vec<u8>,
}

impl std::fmt::Debug for Waffo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Waffo({} mode)", self.mode())
    }
}

impl Waffo {
    /// `test` or `live`, as stored on subscriptions and shown by API-032.
    pub fn mode(&self) -> &'static str {
        if self.live { "live" } else { "test" }
    }

    pub fn product(&self, plan: &str) -> &str {
        if plan == "yearly" {
            &self.product_yearly
        } else {
            &self.product_monthly
        }
    }
}

/// App Store in-app purchases (ADR-043).
#[derive(Clone)]
pub struct Apple {
    pub bundle_id: String,
    /// The App's numeric Apple ID; production notifications carry it.
    pub app_id: i64,
    pub product_monthly: String,
    pub product_yearly: String,
    /// The In-App Purchase key that signs App Store Server API requests.
    pub key_id: String,
    pub issuer_id: String,
    /// Its PKCS#8 DER (P-256), checked at startup.
    pub key: Vec<u8>,
    /// The root (DER) that signed transactions and notifications chain to.
    pub root_cert: Vec<u8>,
    /// Status queries go to `<api_url>/production` and `/sandbox` instead of
    /// Apple's hosts; local tests only.
    pub api_url: Option<String>,
}

impl std::fmt::Debug for Apple {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Apple({})", self.bundle_id)
    }
}

impl Apple {
    /// The plan a product ID sells, if it is one of the two.
    pub fn plan(&self, product: &str) -> Option<&'static str> {
        if product == self.product_monthly {
            Some("monthly")
        } else if product == self.product_yearly {
            Some("yearly")
        } else {
            None
        }
    }
}

const APPLE_ROOT_CA_G3: &[u8] = include_bytes!("../assets/AppleRootCA-G3.cer");

/// All seven IMBO_APPLE_* or none. IMBO_APPLE_ROOT_CERT and IMBO_APPLE_API_URL
/// count only with IMBO_DEV=1.
fn apple_from_env(dev: bool) -> anyhow::Result<Option<Apple>> {
    let names = [
        "IMBO_APPLE_BUNDLE_ID",
        "IMBO_APPLE_APP_ID",
        "IMBO_APPLE_PRODUCT_MONTHLY",
        "IMBO_APPLE_PRODUCT_YEARLY",
        "IMBO_APPLE_KEY_ID",
        "IMBO_APPLE_ISSUER_ID",
        "IMBO_APPLE_PRIVATE_KEY",
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
    let mut next = || v.next().unwrap_or_default();
    let bundle_id = next();
    let app_id = next()
        .parse()
        .context("IMBO_APPLE_APP_ID must be the App's numeric Apple ID")?;
    let (product_monthly, product_yearly, key_id, issuer_id) = (next(), next(), next(), next());
    let key = pem_der(&next())
        .filter(|der| {
            EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, der, &SystemRandom::new())
                .is_ok()
        })
        .context("IMBO_APPLE_PRIVATE_KEY is not an In-App Purchase key (the .p8 file's PEM)")?;
    let root_cert = match var("IMBO_APPLE_ROOT_CERT").filter(|_| dev) {
        Some(path) => std::fs::read_to_string(&path)
            .ok()
            .and_then(|pem| pem_der(&pem))
            .with_context(|| format!("IMBO_APPLE_ROOT_CERT={path} is not a PEM certificate"))?,
        None => APPLE_ROOT_CA_G3.to_vec(),
    };
    Ok(Some(Apple {
        bundle_id,
        app_id,
        product_monthly,
        product_yearly,
        key_id,
        issuer_id,
        key,
        root_cert,
        api_url: var("IMBO_APPLE_API_URL")
            .filter(|_| dev)
            .map(|u| u.trim_end_matches('/').to_string()),
    }))
}

// Waffo's webhook signing keys, as published in @waffo/pancake-ts 0.26.0.
const WAFFO_TEST_WEBHOOK_KEY: &str = "-----BEGIN PUBLIC KEY-----
MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAxnmRY6yMMA3lVqmAU6ZG
b1sjL/+r/z6E+ZjkXaDAKiqOhk9rpazni0bNsGXwmftTPk9jy2wn+j6JHODD/WH/
SCnSfvKkLIjy4Hk7BuCgB174C0ydan7J+KgXLkOwgCAxxB68t2tezldwo74ZpXgn
F49opzMvQ9prEwIAWOE+kV9iK6gx/AckSMtHIHpUesoPDkldpmFHlB2qpf1vsFTZ
5kD6DmGl+2GIVK01aChy2lk8pLv0yUMu18v44sLkO5M44TkGPJD9qG09wrvVG2wp
OTVCn1n5pP8P+HRLcgzbUB3OlZVfdFurn6EZwtyL4ZD9kdkQ4EZE/9inKcp3c1h4
xwIDAQAB
-----END PUBLIC KEY-----";
const WAFFO_PROD_WEBHOOK_KEY: &str = "-----BEGIN PUBLIC KEY-----
MIIBIjANBgkqhkiG9w0BAQEFAAOCAQ8AMIIBCgKCAQEAz+xApdTIb4ua+DgZKQ54
iBsD82ybyhGCLRETONW4Jgbb3A8DUM1LqBk6r/CmTOCHqLalTQHNigvP3R5zkDNX
iRJz6gA4MJ/+8K0+mnEE2RISQzN+Qu65TNd6svb+INm/kMaftY4uIXr6y6kchtTJ
dwnQhcKdAL2v7h7IFnkVelQsKxDdb2PqX8xX/qwd01iXvMcpCCaXovUwZsxH2QN5
ZKBTseJivbhUeyJCco4fdUyxOMHe2ybCVhyvim2uxAl1nkvL5L8RCWMCAV55LLo0
9OhmLahz/DYNu13YLVP6dvIT09ZFBYU6Owj1NxdinTynlJCFS9VYwBgmftosSE1U
dwIDAQAB
-----END PUBLIC KEY-----";

fn waffo_from_env() -> anyhow::Result<Option<Waffo>> {
    let names = [
        "IMBO_WAFFO_MERCHANT_ID",
        "IMBO_WAFFO_STORE_ID",
        "IMBO_WAFFO_PRIVATE_KEY",
        "IMBO_WAFFO_ENV",
        "IMBO_WAFFO_PRODUCT_MONTHLY",
        "IMBO_WAFFO_PRODUCT_YEARLY",
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
    let mut next = || v.next().unwrap_or_default();
    let (merchant_id, store_id, key, env) = (next(), next(), next(), next());
    let live = match env.as_str() {
        "prod" => true,
        "test" => false,
        _ => bail!("IMBO_WAFFO_ENV must be test or prod"),
    };
    let key = pem_der(&key)
        .filter(|der| rsa_key_pair(der).is_some())
        .context("IMBO_WAFFO_PRIVATE_KEY is not an RSA private key (PEM or base64)")?;
    // Local tests sign their own webhooks; only the test environment allows that.
    let webhook_pem = match var("IMBO_WAFFO_WEBHOOK_PUBLIC_KEY") {
        Some(k) if !live => k,
        _ if live => WAFFO_PROD_WEBHOOK_KEY.to_string(),
        _ => WAFFO_TEST_WEBHOOK_KEY.to_string(),
    };
    let webhook_key = pem_der(&webhook_pem)
        .and_then(|der| spki_rsa(&der))
        .context("IMBO_WAFFO_WEBHOOK_PUBLIC_KEY is not an RSA public key (PEM)")?;
    Ok(Some(Waffo {
        merchant_id,
        store_id,
        key,
        live,
        product_monthly: next(),
        product_yearly: next(),
        webhook_key,
    }))
}

pub fn rsa_key_pair(der: &[u8]) -> Option<RsaKeyPair> {
    RsaKeyPair::from_pkcs8(der)
        .or_else(|_| RsaKeyPair::from_der(der))
        .ok()
}

/// The DER inside a PEM block, or a bare base64 key; `\n` may stand for newlines.
fn pem_der(raw: &str) -> Option<Vec<u8>> {
    let b64: String = raw
        .replace("\\n", "\n")
        .split("-----")
        .step_by(2)
        .flat_map(str::chars)
        .filter(|c| !c.is_whitespace())
        .collect();
    STANDARD.decode(b64).ok()
}

/// The RSAPublicKey inside a SubjectPublicKeyInfo; ring verifies with the former.
fn spki_rsa(der: &[u8]) -> Option<Vec<u8>> {
    let (spki, _) = der_item(der, 0x30)?;
    let (_algorithm, rest) = der_item(spki, 0x30)?;
    let (bits, _) = der_item(rest, 0x03)?;
    let (&unused, key) = bits.split_first()?;
    (unused == 0).then(|| key.to_vec())
}

/// The contents of the DER item at the start of `der` if its tag is `tag`,
/// and what follows it.
fn der_item(der: &[u8], tag: u8) -> Option<(&[u8], &[u8])> {
    let (&t, rest) = der.split_first()?;
    let (&first, rest) = rest.split_first()?;
    let (len, rest) = if first < 0x80 {
        (first as usize, rest)
    } else {
        let n = (first & 0x7f) as usize;
        if n == 0 || n > 4 || rest.len() < n {
            return None;
        }
        let len = rest[..n]
            .iter()
            .fold(0usize, |acc, b| acc << 8 | *b as usize);
        (len, &rest[n..])
    };
    (t == tag && rest.len() >= len).then(|| rest.split_at(len))
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
        let dev = flag("IMBO_DEV");
        let waffo = waffo_from_env()?;
        let apple = apple_from_env(dev)?.filter(|_| waffo.is_some());
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
            dev,
            backup_stamp: var("IMBO_BACKUP_STAMP").map(PathBuf::from),
            min_client_version: var("IMBO_MIN_CLIENT_VERSION").unwrap_or_else(|| "0.1.0".into()),
            waffo,
            apple,
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

    /// RULE-007: charging needs both Waffo and a start day.
    pub fn billing_on(&self) -> bool {
        self.waffo.is_some() && self.billing_start.is_some()
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The modulus and exponent of an RSAPublicKey.
    fn rsa_parts(der: &[u8]) -> Option<(&[u8], &[u8])> {
        let (key, _) = der_item(der, 0x30)?;
        let (n, rest) = der_item(key, 0x02)?;
        let (e, _) = der_item(rest, 0x02)?;
        Some((n, e))
    }

    #[test]
    fn webhook_keys_are_rsa_2048() {
        for pem in [WAFFO_TEST_WEBHOOK_KEY, WAFFO_PROD_WEBHOOK_KEY] {
            let key = pem_der(pem).and_then(|der| spki_rsa(&der)).unwrap();
            let (n, e) = rsa_parts(&key).unwrap();
            // A leading zero, then 2048 bits.
            assert_eq!(n.len(), 257);
            assert_eq!(e, [1, 0, 1]);
        }
    }

    #[test]
    fn embedded_root_is_apple_root_ca_g3() {
        use sha2::{Digest, Sha256};
        assert_eq!(
            hex::encode(Sha256::digest(APPLE_ROOT_CA_G3)),
            "63343abfb89a6a03ebb57e9b3f5fa7be7c4f5c756f3017b3a8c488c3653e9179"
        );
    }

    #[test]
    fn pem_bare_and_escaped_read_the_same() {
        let der = pem_der(WAFFO_TEST_WEBHOOK_KEY).unwrap();
        let bare: String = WAFFO_TEST_WEBHOOK_KEY
            .lines()
            .filter(|l| !l.starts_with("-----"))
            .collect();
        assert_eq!(pem_der(&bare), Some(der.clone()));
        let escaped = WAFFO_TEST_WEBHOOK_KEY.replace('\n', "\\n");
        assert_eq!(pem_der(&escaped), Some(der));
    }
}
