//! Connect to Postgres: `DATABASE_URL` when set, otherwise an embedded
//! PostgreSQL in the data directory (self-hosting default, ADR-003).

use std::time::Duration;

use anyhow::Context;
use sqlx::PgPool;
use sqlx::postgres::PgPoolOptions;
use tracing::info;

use crate::config::Config;

pub struct Database {
    pub pool: PgPool,
    pub kind: &'static str,
    #[cfg(feature = "embedded-db")]
    embedded: Option<oliphaunt::AsyncOliphauntServer>,
}

impl Database {
    pub async fn close(self) {
        self.pool.close().await;
        #[cfg(feature = "embedded-db")]
        if let Some(server) = self.embedded
            && let Err(e) = server.close().await
        {
            tracing::warn!(error = %e, "embedded postgres did not stop cleanly");
        }
    }
}

async fn pool(url: &str) -> anyhow::Result<PgPool> {
    PgPoolOptions::new()
        .max_connections(10)
        .acquire_timeout(Duration::from_secs(5))
        .connect(url)
        .await
        .context("connect to postgres")
}

pub async fn open(cfg: &Config) -> anyhow::Result<Database> {
    if let Some(url) = &cfg.database_url {
        return Ok(Database {
            pool: pool(url).await?,
            kind: "postgres",
            #[cfg(feature = "embedded-db")]
            embedded: None,
        });
    }
    open_embedded(cfg).await
}

#[cfg(feature = "embedded-db")]
async fn open_embedded(cfg: &Config) -> anyhow::Result<Database> {
    use oliphaunt::{AsyncOliphauntServer, DatabaseStorage, ServerListen};
    if unsafe { libc::geteuid() } == 0 {
        anyhow::bail!(
            "the embedded database cannot run as root; run imbo-server as a normal user or set DATABASE_URL"
        );
    }
    let dir = cfg.data_dir.join("pg");
    std::fs::create_dir_all(&dir)?;
    let dir = dir.canonicalize()?;
    info!(dir = %dir.display(), "starting embedded postgres");
    let server = AsyncOliphauntServer::builder()
        .storage(DatabaseStorage::Directory(dir))
        .listen(ServerListen::tcp())
        .start()
        .await
        .context("start embedded postgres")?;
    let pool = pool(server.connection_string()).await?;
    Ok(Database {
        pool,
        kind: "embedded",
        embedded: Some(server),
    })
}

#[cfg(not(feature = "embedded-db"))]
async fn open_embedded(_cfg: &Config) -> anyhow::Result<Database> {
    anyhow::bail!("DATABASE_URL is required (this build has no embedded database)")
}
