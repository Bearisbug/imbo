use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sqlx::PgPool;
use uuid::Uuid;

use crate::config::Config;
use crate::hub::Hub;
use crate::limits::Limiter;
use crate::mail::Mailer;

pub struct AppState {
    pub cfg: Config,
    pub db: PgPool,
    pub hub: Hub,
    pub mailer: Mailer,
    pub limiter: Limiter,
    /// For Creem's API.
    pub http: reqwest::Client,
    /// Payment webhooks that failed the signature or processing (API-025).
    webhook_errors: Mutex<VecDeque<Instant>>,
    /// Serialises pushes per receiving agent so frames leave in message order.
    push_locks: Mutex<HashMap<Uuid, Arc<tokio::sync::Mutex<()>>>>,
}

pub type St = Arc<AppState>;

impl AppState {
    pub fn new(cfg: Config, db: PgPool) -> anyhow::Result<Self> {
        let mailer = Mailer::new(&cfg)?;
        Ok(Self {
            cfg,
            db,
            hub: Hub::default(),
            mailer,
            limiter: Limiter::default(),
            http: crate::billing::http_client(),
            webhook_errors: Mutex::new(VecDeque::new()),
            push_locks: Mutex::new(HashMap::new()),
        })
    }

    /// Attachment content, `<id>` once complete and `<id>.part` while uploading.
    pub fn files_dir(&self) -> PathBuf {
        self.cfg.data_dir.join("files")
    }

    pub fn note_webhook_error(&self) {
        let mut q = self.webhook_errors.lock().unwrap();
        q.push_back(Instant::now());
        if q.len() > 10_000 {
            q.pop_front();
        }
    }

    /// Webhook errors in the last 24 hours, counted since this process started.
    pub fn webhook_errors_24h(&self) -> i64 {
        let day = Duration::from_secs(24 * 3600);
        let mut q = self.webhook_errors.lock().unwrap();
        while q.front().is_some_and(|t| t.elapsed() > day) {
            q.pop_front();
        }
        q.len() as i64
    }

    pub fn push_lock(&self, agent: Uuid) -> Arc<tokio::sync::Mutex<()>> {
        let mut map = self.push_locks.lock().unwrap();
        if map.len() > 10_000 {
            map.retain(|_, l| Arc::strong_count(l) > 1);
        }
        map.entry(agent).or_default().clone()
    }
}
