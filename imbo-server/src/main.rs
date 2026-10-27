//! `imbo-server`: one binary for the central server and for self-hosting.

mod auth;
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
mod routes;
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
    /// Payment records (Creem).
    Billing {
        #[command(subcommand)]
        cmd: BillingCmd,
    },
}

#[derive(Subcommand)]
enum BillingCmd {
    /// Fetch a subscription from Creem and record it, for a webhook that never
    /// arrived (RB-005). Uses the server's environment and DATABASE_URL.
    Pull { subscription_id: String },
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
                cmd: BillingCmd::Pull { subscription_id },
            } => billing_pull(&subscription_id).await,
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

async fn billing_pull(id: &str) -> anyhow::Result<()> {
    let cfg = config::Config::from_env()?;
    let creem = cfg
        .creem
        .clone()
        .context("Creem is not configured (IMBO_CREEM_API_KEY and the rest)")?;
    if cfg.database_url.is_none() {
        anyhow::bail!("billing pull needs DATABASE_URL");
    }
    let database = db::open(&cfg).await?;
    let sub = billing::fetch_subscription(&billing::http_client(), &creem, id)
        .await
        .map_err(|e| anyhow::anyhow!("{}", e.0.detail))?;
    let mut tx = database.pool.begin().await?;
    let outcome =
        billing::upsert_subscription(&mut tx, &creem, &sub, false, chrono::Utc::now()).await?;
    tx.commit().await?;
    println!(
        "{id}: {} (status {}, paid until {})",
        match outcome {
            billing::Outcome::Applied => "recorded",
            billing::Outcome::Stale => "already up to date",
            billing::Outcome::UnknownAccount => "no account in its metadata, not recorded",
            billing::Outcome::UnknownProduct => "not one of the two plans, not recorded",
            _ => "not recorded",
        },
        sub["status"].as_str().unwrap_or("?"),
        sub["current_period_end_date"].as_str().unwrap_or("?"),
    );
    database.close().await;
    Ok(())
}

async fn wait_for_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    let mut term = signal(SignalKind::terminate()).expect("signal handler");
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
}
