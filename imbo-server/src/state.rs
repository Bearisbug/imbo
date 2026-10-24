use std::collections::HashMap;
use std::sync::{Arc, Mutex};

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
            push_locks: Mutex::new(HashMap::new()),
        })
    }

    pub fn push_lock(&self, agent: Uuid) -> Arc<tokio::sync::Mutex<()>> {
        let mut map = self.push_locks.lock().unwrap();
        if map.len() > 10_000 {
            map.retain(|_, l| Arc::strong_count(l) > 1);
        }
        map.entry(agent).or_default().clone()
    }
}
