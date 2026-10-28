//! `imbo-server`: one binary for the central server and for self-hosting.

mod auth;
mod avatar;
mod billing;
mod config;
mod db;
mod delivery;
mod dev;
mod error;
mod hub;
mod limits;
mod mail;
mod notify;
mod push;
mod routes;
mod safety;
mod state;
mod views;
mod ws;

use std::net::SocketAddr;
use std::sync::Arc;

use anyhow::Context;
use clap::{Parser, Subcommand};
use tokio_stream::StreamExt;
use tracing::{error, info};

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Parser)]
#[command(name = "imbo-server", version, about = "Imbo server")]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the server (default).
    Serve,
    /// Development helpers; the running server must have IMBO_DEV=1.
    Dev {
        #[command(subcommand)]
        cmd: dev::DevCmd,
    },
    /// Payment records (Waffo).
    Billing {
        #[command(subcommand)]
        cmd: BillingCmd,
    },
    /// Ban or unban an account, as the web admin page does (API-066,
    /// API-067). Uses the server's environment and its database.
    Account {
        #[command(subcommand)]
        cmd: AccountCmd,
    },
}

#[derive(Subcommand)]
enum AccountCmd {
    /// Ban: end every sign-in, take back undelivered messages, close pending
    /// requests and cancel the subscription without a refund. Run it again to
    /// retry a cancellation that failed.
    Ban { account_id: i32 },
    /// Lift a ban. Ended sign-ins and canceled subscriptions stay that way.
    Unban { account_id: i32 },
}

#[derive(Subcommand)]
enum BillingCmd {
    /// Fetch a subscription order from Waffo and record it, for a webhook that
    /// never arrived (RB-005). Uses the server's environment and DATABASE_URL.
    Pull { order_id: String },
    /// Revoke a subscription: no access from now on, and cancel it at Waffo
    /// without a refund (chargebacks, RB-007). Run it again if the
    /// cancellation fails.
    Revoke { order_id: String },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,sqlx=warn,rustls_acme=info".into()),
        )
        .with_target(false)
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
        .init();
    let _ = rustls::crypto::ring::default_provider().install_default();
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(async move {
        match cli.cmd.unwrap_or(Cmd::Serve) {
            Cmd::Serve => serve().await,
            Cmd::Dev { cmd } => dev::run_cli(cmd).await,
            Cmd::Billing {
                cmd: BillingCmd::Pull { order_id },
            } => billing_pull(&order_id).await,
            Cmd::Billing {
                cmd: BillingCmd::Revoke { order_id },
            } => billing_revoke(&order_id).await,
            Cmd::Account { cmd } => account_cmd(cmd).await,
        }
    })
}

async fn serve() -> anyhow::Result<()> {
    let cfg = config::Config::from_env()?;
    std::fs::create_dir_all(&cfg.data_dir)
        .with_context(|| format!("create data dir {}", cfg.data_dir.display()))?;
    let database = db::open(&cfg).await?;
    // After a rollback the database may hold migrations from a newer build;
    // they only add tables and columns, so this build still starts (ADR-024).
    let mut migrator = sqlx::migrate!("./migrations");
    migrator.set_ignore_missing(true);
    migrator
        .run(&database.pool)
        .await
        .context("run migrations")?;
    let owners = db::fill_group_owners(&database.pool).await?;
    if owners > 0 {
        info!(groups = owners, "filled in group owners");
    }
    // Every device connection is gone after a restart.
    sqlx::query("UPDATE agents SET online = false, last_online_at = now() WHERE online")
        .execute(&database.pool)
        .await?;
    info!(version = VERSION, db = database.kind, "database ready");

    let state = Arc::new(state::AppState::new(cfg.clone(), database.pool.clone())?);
    delivery::spawn_tasks(state.clone());
    safety::spawn_ban_listener(state.clone());
    let app = routes::router(state.clone());

    let listen: SocketAddr = cfg.listen.parse().context("IMBO_LISTEN")?;
    dev::write_server_file(&cfg, &listen)?;

    let shutdown = axum_server::Handle::new();
    {
        let h = shutdown.clone();
        tokio::spawn(async move {
            wait_for_signal().await;
            info!("shutting down");
            h.graceful_shutdown(Some(std::time::Duration::from_secs(5)));
        });
    }

    let make = app.into_make_service_with_connect_info::<SocketAddr>();
    let result = if let Some(domain) = cfg.domain.clone().filter(|_| cfg.tls) {
        let mut acme = rustls_acme::AcmeConfig::new([domain.clone()])
            .contact(cfg.acme_email.iter().map(|e| format!("mailto:{e}")))
            .cache(rustls_acme::caches::DirCache::new(
                cfg.data_dir.join("acme"),
            ))
            .directory_lets_encrypt(!cfg.acme_staging)
            .state();
        let acceptor = acme.axum_acceptor(acme.default_rustls_config());
        tokio::spawn(async move {
            while let Some(ev) = acme.next().await {
                match ev {
                    Ok(ok) => info!(event = ?ok, "acme"),
                    Err(err) => error!(error = ?err, "acme"),
                }
            }
        });
        info!(%listen, %domain, "listening with automatic TLS");
        axum_server::bind(listen)
            .handle(shutdown)
            .acceptor(acceptor)
            .serve(make)
            .await
    } else {
        info!(%listen, "listening (plain HTTP)");
        axum_server::bind(listen).handle(shutdown).serve(make).await
    };
    result.context("server")?;
    database.close().await;
    Ok(())
}

/// The Waffo settings and database the billing commands work on.
async fn billing_setup() -> anyhow::Result<(config::Waffo, db::Database)> {
    let cfg = config::Config::from_env()?;
    let waffo = cfg
        .waffo
        .clone()
        .context("Waffo is not configured (IMBO_WAFFO_MERCHANT_ID and the rest)")?;
    if cfg.database_url.is_none() {
        anyhow::bail!("billing commands need DATABASE_URL");
    }
    let database = db::open(&cfg).await?;
    Ok((waffo, database))
}

/// API-035: also cancels an order that still renews when its account is
/// deleted or banned; the record stands even when Waffo can't cancel.
async fn billing_pull(id: &str) -> anyhow::Result<()> {
    let (waffo, database) = billing_setup().await?;
    let http = billing::http_client();
    let order = billing::fetch_order(&http, &waffo, id)
        .await
        .map_err(|e| anyhow::anyhow!("{}", e.0.detail))?;
    let mut tx = database.pool.begin().await?;
    let outcome = billing::record_pulled(&mut tx, &waffo, &order).await?;
    let renews = matches!(
        order["status"].as_str(),
        Some("active" | "trialing" | "past_due")
    ) && matches!(
        outcome,
        Some(billing::Outcome::Applied | billing::Outcome::Stale)
    );
    let gone = if renews {
        billing::gone_account(&mut tx, id).await?
    } else {
        None
    };
    tx.commit().await?;
    println!(
        "{id}: {} (status {}, period ends {})",
        match outcome {
            Some(billing::Outcome::Applied) => "recorded",
            Some(billing::Outcome::Stale) => "already up to date",
            Some(billing::Outcome::UnknownAccount) => "no account in its metadata, not recorded",
            Some(billing::Outcome::UnknownProduct) => "not one of the two plans, not recorded",
            _ => "a status that is never recorded",
        },
        order["status"].as_str().unwrap_or("?"),
        order["currentPeriodEnd"].as_str().unwrap_or("?"),
    );
    let result = match gone {
        None => Ok(()),
        Some(account) => billing::cancel_order(&http, &waffo, id)
            .await
            .map(|()| {
                println!(
                    "{id}: account {account} is deleted or banned; canceled at Waffo, it won't renew."
                )
            })
            .map_err(|e| {
                anyhow::anyhow!(
                    "{id}: account {account} is deleted or banned but Waffo did not cancel it ({}); run this again",
                    e.0.detail
                )
            }),
    };
    database.close().await;
    result
}

/// RB-007: the revocation stands even when Waffo can't cancel.
async fn billing_revoke(id: &str) -> anyhow::Result<()> {
    if !billing::order_id_ok(id) {
        anyhow::bail!("An order ID looks like ORD_…");
    }
    let (waffo, database) = billing_setup().await?;
    let mut tx = database.pool.begin().await?;
    let account = billing::revoke(&mut tx, id).await?;
    tx.commit().await?;
    let result = match account {
        None => Err(anyhow::anyhow!(
            "no subscription {id} is recorded; run `imbo-server billing pull {id}` first"
        )),
        Some(account) => {
            println!("{id}: revoked; account {account} has no access from it any more.");
            billing::cancel_order(&billing::http_client(), &waffo, id)
                .await
                .map(|()| println!("{id}: canceled at Waffo, it won't renew."))
                .map_err(|e| {
                    anyhow::anyhow!(
                        "{id} is revoked but Waffo did not cancel it ({}); run this again",
                        e.0.detail
                    )
                })
        }
    };
    database.close().await;
    result
}

/// `imbo-server account ban|unban`. With the embedded database the server
/// must be stopped first; with DATABASE_URL it may keep running, and closes
/// the banned account's connections when told (safety::spawn_ban_listener).
async fn account_cmd(cmd: AccountCmd) -> anyhow::Result<()> {
    let cfg = config::Config::from_env()?;
    let database = db::open(&cfg).await?;
    let pool = &database.pool;
    let result = async {
        match cmd {
            AccountCmd::Ban { account_id } => {
                let row: Option<(String, String)> = sqlx::query_as(
                    "SELECT name, email FROM accounts a WHERE id = $1
                     AND NOT EXISTS (SELECT 1 FROM deleted_accounts d WHERE d.id = a.id)",
                )
                .bind(account_id)
                .fetch_optional(pool)
                .await?;
                let (name, email) = row.with_context(|| format!("no account {account_id}"))?;
                if cfg.is_admin_email(&email) {
                    anyhow::bail!("{account_id} is an admin (IMBO_ADMIN_EMAILS); admins can't be banned");
                }
                let mut tx = pool.begin().await?;
                let b = safety::ban_tx(&mut tx, account_id, None)
                    .await
                    .map_err(|e| anyhow::anyhow!("{}", e.0.detail))?;
                tx.commit().await?;
                let canceled =
                    safety::after_ban(pool, &cfg, &billing::http_client(), account_id, b.newly)
                        .await;
                if b.newly {
                    println!(
                        "Banned {name} ({account_id}): {} computer(s) signed out, {} undelivered message(s) taken back.",
                        b.devices.len(),
                        b.canceled.len()
                    );
                } else {
                    println!("{name} ({account_id}) was already banned.");
                }
                println!(
                    "Subscription canceled: {}",
                    if canceled { "yes" } else { "no, run this again to retry" }
                );
            }
            AccountCmd::Unban { account_id } => {
                safety::unban(pool, account_id)
                    .await
                    .map_err(|e| anyhow::anyhow!("{}", e.0.detail))?;
                println!("Unbanned {account_id}. They sign in again; their subscription is not restored.");
            }
        }
        anyhow::Ok(())
    }
    .await;
    database.close().await;
    result
}

async fn wait_for_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = signal(SignalKind::terminate()).expect("signal handler");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
}
