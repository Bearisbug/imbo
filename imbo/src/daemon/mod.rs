//! `imbo daemon`: one per computer. Holds the device token, keeps the
//! WebSocket to the server, tracks sessions and puts messages into them.

mod dedupe;
mod files;
mod link;
mod sessions;
mod socket;

use std::collections::{HashMap, HashSet, VecDeque};
use std::os::fd::AsRawFd;
use std::sync::{Arc, Mutex, RwLock};
use std::time::Duration;

use anyhow::Context;
use imbo_proto::frames::{ClientFrame, Delivery, Notice};
use tokio::sync::{Notify, mpsc};
use tracing::{info, warn};
use uuid::Uuid;

use crate::adapters;
use crate::paths::{Credentials, Home};
use sessions::{Key, Sessions};

#[derive(Default)]
pub struct LinkState {
    pub connected: bool,
    pub revoked: bool,
    pub last_error: Option<String>,
}

pub struct Daemon {
    pub home: Home,
    creds: RwLock<Option<Credentials>>,
    pub sessions: Mutex<Sessions>,
    out: Mutex<Option<mpsc::UnboundedSender<ClientFrame>>>,
    pub link: Mutex<LinkState>,
    pub notices: Mutex<VecDeque<Notice>>,
    dedupe: Mutex<dedupe::Dedupe>,
    queues: Mutex<HashMap<Key, mpsc::UnboundedSender<Delivery>>>,
    canceled: Mutex<HashSet<(i64, Uuid)>>,
    uploads: Mutex<files::Recent>,
    fetch_locks: Mutex<HashMap<i64, Arc<tokio::sync::Mutex<()>>>>,
    pub reload: Notify,
    pub presence_dirty: Notify,
    pub shutdown: Notify,
}

pub type D = Arc<Daemon>;

impl Daemon {
    pub fn creds(&self) -> Option<Credentials> {
        self.creds.read().unwrap().clone()
    }

    pub fn reload_creds(&self) {
        *self.creds.write().unwrap() = self.home.credentials();
        self.uploads.lock().unwrap().clear();
        self.link.lock().unwrap().revoked = false;
        self.reload.notify_waiters();
    }

    pub fn send_frame(&self, f: ClientFrame) -> bool {
        match self.out.lock().unwrap().as_ref() {
            Some(tx) => tx.send(f).is_ok(),
            None => false,
        }
    }

    pub fn push_notice(&self, n: Notice) {
        info!(kind = %n.kind, note = n.note.as_deref().unwrap_or(""), "notice: {}", n.text);
        let mut q = self.notices.lock().unwrap();
        q.push_front(n);
        q.truncate(20);
    }

    /// Send presence changes now (if connected).
    pub fn flush_presence(&self) {
        let changes = self.sessions.lock().unwrap().changes();
        if !changes.is_empty() {
            for c in &changes {
                info!(tool = %c.tool, session = %c.session_id, title = %c.title, online = c.online, "presence");
            }
            self.send_frame(ClientFrame::Presence { sessions: changes });
        }
    }

    fn enqueue(self: &Arc<Self>, d: Delivery) {
        let key = (d.tool.clone(), d.session_id.clone());
        let mut queues = self.queues.lock().unwrap();
        if let Some(tx) = queues.get(&key)
            && tx.send(d.clone()).is_ok()
        {
            return;
        }
        let (tx, rx) = mpsc::unbounded_channel();
        let _ = tx.send(d);
        queues.insert(key.clone(), tx);
        let me = self.clone();
        tokio::spawn(async move { me.worker(key, rx).await });
    }

    fn ack(&self, d: &Delivery, via: &str) {
        self.dedupe.lock().unwrap().add(d.message.id, d.agent_id);
        self.send_frame(ClientFrame::Ack {
            message_id: d.message.id,
            agent_id: d.agent_id,
            via: via.into(),
        });
    }

    fn nack(&self, d: &Delivery, reason: &str) {
        self.send_frame(ClientFrame::Nack {
            message_id: d.message.id,
            agent_id: d.agent_id,
            reason: reason.into(),
        });
    }

    /// Attachments first, so the message can name local paths (ADR-012).
    async fn download(self: &Arc<Self>, d: &Delivery) -> Vec<imbo_proto::local::LocalFile> {
        if d.message.files.is_empty() {
            return vec![];
        }
        files::fetch(self, &d.message, None).await
    }

    /// One queue per session so its messages go in one at a time, in order.
    async fn worker(self: Arc<Self>, key: Key, mut rx: mpsc::UnboundedReceiver<Delivery>) {
        while let Some(d) = rx.recv().await {
            let id = (d.message.id, d.agent_id);
            if self.canceled.lock().unwrap().remove(&id) {
                info!(msg = id.0, "skipped canceled delivery");
                continue;
            }
            if self.dedupe.lock().unwrap().contains(id.0, id.1) {
                self.ack(&d, "inject");
                continue;
            }
            let entry = self.sessions.lock().unwrap().get(&key).cloned();
            let Some(entry) = entry.filter(|e| e.online) else {
                self.nack(&d, "session-offline");
                continue;
            };
            let Some(tool) = adapters::get(&key.0).filter(|_| !entry.fake) else {
                self.nack(&d, "no-channel");
                continue;
            };
            if let Err(reason) = tool.ready(&key.1) {
                self.nack(&d, reason);
                continue;
            }
            let files = self.download(&d).await;
            match tool.deliver(&key.1, &d.message, &files).await {
                Ok(()) => self.ack(&d, "inject"),
                Err(e) => self.nack(&d, &format!("inject-failed: {e}")),
            }
        }
    }
}

fn lock_single_instance(home: &Home) -> anyhow::Result<std::fs::File> {
    let f = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(home.dir.join("daemon.lock"))
        .context("open daemon.lock")?;
    if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        anyhow::bail!(
            "another imbo daemon is already running for {}",
            home.dir.display()
        );
    }
    Ok(f)
}

pub async fn run() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("IMBO_LOG")
                .unwrap_or_else(|_| "info".into()),
        )
        .with_ansi(false)
        .with_target(false)
        .init();
    let home = crate::paths::Home::resolve();
    home.ensure()?;
    let _lock = match lock_single_instance(&home) {
        Ok(l) => l,
        Err(e) => {
            info!("{e}");
            return Ok(());
        }
    };
    std::fs::write(home.dir.join("daemon.pid"), std::process::id().to_string())?;
    let daemon = Arc::new(Daemon {
        creds: RwLock::new(home.credentials()),
        dedupe: Mutex::new(dedupe::Dedupe::load(home.state().join("delivered.log"))),
        home: home.clone(),
        sessions: Mutex::new(Sessions::default()),
        out: Mutex::new(None),
        link: Mutex::new(LinkState::default()),
        notices: Mutex::new(VecDeque::new()),
        queues: Mutex::new(HashMap::new()),
        canceled: Mutex::new(HashSet::new()),
        uploads: Mutex::new(HashMap::new()),
        fetch_locks: Mutex::new(HashMap::new()),
        reload: Notify::new(),
        presence_dirty: Notify::new(),
        shutdown: Notify::new(),
    });
    info!(version = crate::VERSION, home = %home.dir.display(), "imbo daemon starting");
    let listener = socket::bind(&home)?;
    tokio::spawn(socket::serve(daemon.clone(), listener));
    tokio::spawn(monitor(daemon.clone()));
    tokio::spawn(link::run(daemon.clone()));

    use tokio::signal::unix::{SignalKind, signal};
    let mut term = signal(SignalKind::terminate())?;
    tokio::select! {
        _ = term.recv() => {}
        _ = tokio::signal::ctrl_c() => {}
        _ = daemon.shutdown.notified() => {}
    }
    info!("imbo daemon stopping");
    let _ = std::fs::remove_file(home.socket());
    let _ = std::fs::remove_file(home.dir.join("daemon.pid"));
    Ok(())
}

/// Session discovery: each tool as often as its adapter asks, counted in
/// ticks of 2.5 s.
async fn monitor(d: D) {
    const TICK: Duration = Duration::from_millis(2500);
    let mut tick: u64 = 0;
    loop {
        for a in adapters::all() {
            let every = (a.poll_every().as_millis() / TICK.as_millis()).max(1) as u64;
            if !tick.is_multiple_of(every) {
                continue;
            }
            match a.discover().await {
                Ok(found) => d.sessions.lock().unwrap().apply(a.id(), found),
                Err(e) => warn!(tool = a.id(), error = %e, "session discovery failed"),
            }
        }
        tick += 1;
        d.flush_presence();
        tokio::select! {
            _ = tokio::time::sleep(TICK) => {}
            _ = d.presence_dirty.notified() => {
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
        }
    }
}
