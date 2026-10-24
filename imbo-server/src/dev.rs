//! Development helpers (`IMBO_DEV=1` only): seed accounts and expire codes.
//! The CLI side talks to the running server found via `server.json` in the
//! data directory, so it works with the embedded database too.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use axum::Json;
use axum::extract::{ConnectInfo, State};
use clap::Subcommand;
use imbo_proto::ErrorCode;
use rand::Rng;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use uuid::Uuid;

use crate::auth::{new_token, token_hash};
use crate::config::Config;
use crate::error::{ApiResult, err};
use crate::routes::Body;
use crate::state::St;

#[derive(Subcommand)]
pub enum DevCmd {
    /// Create alice, bob and carol (or --names) and write client credentials.
    Seed {
        /// Directory that gets one IMBO_HOME per account (<dir>/<name>).
        #[arg(long)]
        homes: PathBuf,
        /// Comma-separated names to create.
        #[arg(long, default_value = "alice,bob,carol")]
        names: String,
        /// Contact pairs, e.g. alice:bob,alice:carol.
        #[arg(long, default_value = "")]
        contacts: String,
        /// Server URL written into the client config (default: this server).
        #[arg(long)]
        server: Option<String>,
    },
    /// Make the email's active code expire now.
    ExpireCode { email: String },
}

#[derive(Serialize, Deserialize)]
struct ServerFile {
    listen: String,
    pid: u32,
    version: String,
}

pub fn write_server_file(cfg: &Config, listen: &SocketAddr) -> anyhow::Result<()> {
    let f = ServerFile {
        listen: listen.to_string(),
        pid: std::process::id(),
        version: crate::VERSION.into(),
    };
    std::fs::write(
        cfg.data_dir.join("server.json"),
        serde_json::to_vec_pretty(&f)?,
    )?;
    Ok(())
}

#[derive(Deserialize)]
pub struct SeedIn {
    accounts: Vec<SeedAccount>,
    #[serde(default)]
    contacts: Vec<(String, String)>,
}

#[derive(Serialize, Deserialize)]
struct SeedAccount {
    name: String,
    email: String,
}

#[derive(Serialize, Deserialize)]
pub struct SeedOut {
    accounts: Vec<SeededAccount>,
}

#[derive(Serialize, Deserialize)]
struct SeededAccount {
    name: String,
    email: String,
    account_id: i32,
    device_id: Uuid,
    token: String,
}

fn local_only(peer: &SocketAddr) -> ApiResult<()> {
    if !peer.ip().is_loopback() {
        return Err(err(
            ErrorCode::Forbidden,
            "Dev endpoints only answer on loopback.",
        ));
    }
    Ok(())
}

pub async fn seed(
    State(st): State<St>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Body(req): Body<SeedIn>,
) -> ApiResult<Json<SeedOut>> {
    local_only(&peer)?;
    let mut out = vec![];
    for a in req.accounts {
        let email = a.email.to_lowercase();
        let existing: Option<i32> =
            sqlx::query_scalar("SELECT id FROM accounts WHERE lower(email) = $1")
                .bind(&email)
                .fetch_optional(&st.db)
                .await?;
        let account_id = match existing {
            Some(id) => id,
            None => loop {
                let id = rand::rng().random_range(10_000_000..=99_999_999);
                let r = sqlx::query("INSERT INTO accounts (id, name, email) VALUES ($1, $2, $3) ON CONFLICT DO NOTHING")
                    .bind(id)
                    .bind(&a.name)
                    .bind(&email)
                    .execute(&st.db)
                    .await?;
                if r.rows_affected() == 1 {
                    break id;
                }
            },
        };
        let token = new_token();
        let device_id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO devices (id, account_id, name, platform, client_version, token_hash) VALUES ($1, $2, 'seed', 'dev', 'dev', $3)",
        )
        .bind(device_id)
        .bind(account_id)
        .bind(token_hash(&token))
        .execute(&st.db)
        .await?;
        out.push(SeededAccount {
            name: a.name,
            email,
            account_id,
            device_id,
            token,
        });
    }
    for (x, y) in req.contacts {
        let id = |n: &str| out.iter().find(|a| a.name == n).map(|a| a.account_id);
        let (Some(a), Some(b)) = (id(&x), id(&y)) else {
            return Err(err(
                ErrorCode::InvalidRequest,
                format!("unknown seed name in {x}:{y}"),
            ));
        };
        sqlx::query("INSERT INTO contacts (account_id, peer_id) VALUES ($1, $2), ($2, $1) ON CONFLICT DO NOTHING")
            .bind(a)
            .bind(b)
            .execute(&st.db)
            .await?;
    }
    Ok(Json(SeedOut { accounts: out }))
}

#[derive(Deserialize)]
pub struct ExpireIn {
    email: String,
}

pub async fn expire_code(
    State(st): State<St>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Body(req): Body<ExpireIn>,
) -> ApiResult<Json<serde_json::Value>> {
    local_only(&peer)?;
    let r = sqlx::query(
        "UPDATE email_codes SET expires_at = now() - interval '1 second' WHERE lower(email) = $1 AND status = 'active'",
    )
    .bind(req.email.to_lowercase())
    .execute(&st.db)
    .await?;
    Ok(Json(serde_json::json!({ "expired": r.rows_affected() })))
}

// ---- CLI side ----

fn data_dir() -> PathBuf {
    std::env::var("IMBO_DATA_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|_| PathBuf::from("imbo-data"))
}

fn server_addr(dir: &Path) -> anyhow::Result<String> {
    let raw = std::fs::read(dir.join("server.json")).with_context(|| {
        format!(
            "no running server found in {} (server.json missing)",
            dir.display()
        )
    })?;
    let f: ServerFile = serde_json::from_slice(&raw)?;
    let addr: SocketAddr = f.listen.parse()?;
    let host = if addr.ip().is_unspecified() {
        "127.0.0.1".to_string()
    } else {
        addr.ip().to_string()
    };
    Ok(format!("{host}:{}", addr.port()))
}

async fn post(
    addr: &str,
    path: &str,
    body: &serde_json::Value,
) -> anyhow::Result<serde_json::Value> {
    let payload = serde_json::to_vec(body)?;
    let mut s = tokio::net::TcpStream::connect(addr)
        .await
        .with_context(|| format!("connect {addr}"))?;
    let head = format!(
        "POST {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        payload.len()
    );
    s.write_all(head.as_bytes()).await?;
    s.write_all(&payload).await?;
    let mut buf = vec![];
    s.read_to_end(&mut buf).await?;
    let text = String::from_utf8_lossy(&buf);
    let (head, body) = text.split_once("\r\n\r\n").context("bad HTTP response")?;
    let status: u16 = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(0);
    if status != 200 {
        bail!("{path} returned {status}: {body} (is IMBO_DEV=1 set on the server?)");
    }
    Ok(serde_json::from_str(body)?)
}

pub async fn run_cli(cmd: DevCmd) -> anyhow::Result<()> {
    let addr = server_addr(&data_dir())?;
    match cmd {
        DevCmd::ExpireCode { email } => {
            let v = post(
                &addr,
                "/v1/dev/expire-code",
                &serde_json::json!({ "email": email }),
            )
            .await?;
            println!("expired {} code(s) for {email}", v["expired"]);
        }
        DevCmd::Seed {
            homes,
            names,
            contacts,
            server,
        } => {
            let names: Vec<&str> = names
                .split(',')
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .collect();
            let accounts: Vec<SeedAccount> = names
                .iter()
                .map(|n| SeedAccount {
                    name: n.to_string(),
                    email: format!("{n}@imbo.test"),
                })
                .collect();
            let pairs: Vec<(String, String)> = contacts
                .split(',')
                .filter_map(|p| p.split_once(':'))
                .map(|(a, b)| (a.trim().to_string(), b.trim().to_string()))
                .collect();
            let v = post(
                &addr,
                "/v1/dev/seed",
                &serde_json::json!({ "accounts": accounts, "contacts": pairs }),
            )
            .await?;
            let out: SeedOut = serde_json::from_value(v)?;
            let server = server.unwrap_or_else(|| format!("http://{addr}"));
            for a in out.accounts {
                let home = homes.join(&a.name);
                std::fs::create_dir_all(&home)?;
                std::fs::write(home.join("config.toml"), format!("server = {:?}\n", server))?;
                let cred = home.join("credentials");
                std::fs::write(
                    &cred,
                    format!(
                        "server = {:?}\ntoken = {:?}\ndevice_id = {:?}\naccount_id = {}\nname = {:?}\nemail = {:?}\n",
                        server,
                        a.token,
                        a.device_id.to_string(),
                        a.account_id,
                        a.name,
                        a.email
                    ),
                )?;
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&cred, std::fs::Permissions::from_mode(0o600))?;
                println!("{}\t{}\t{}", a.name, a.account_id, home.display());
            }
        }
    }
    Ok(())
}
