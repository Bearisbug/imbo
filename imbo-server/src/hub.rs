//! Live device connections (one WebSocket per device).

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use imbo_proto::frames::ServerFrame;
use tokio::sync::mpsc;
use uuid::Uuid;

pub enum Outbound {
    Frame(ServerFrame),
    Close(u16, &'static str),
}

struct Conn {
    id: u64,
    account_id: i32,
    tx: mpsc::UnboundedSender<Outbound>,
}

#[derive(Default)]
pub struct Hub {
    conns: Mutex<HashMap<Uuid, Conn>>,
    next: AtomicU64,
}

impl Hub {
    /// Register a connection. An older connection of the same device is
    /// closed with 4409.
    pub fn register(
        &self,
        device: Uuid,
        account_id: i32,
    ) -> (u64, mpsc::UnboundedReceiver<Outbound>) {
        let (tx, rx) = mpsc::unbounded_channel();
        let id = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        let old = self
            .conns
            .lock()
            .unwrap()
            .insert(device, Conn { id, account_id, tx });
        if let Some(old) = old {
            let _ = old.tx.send(Outbound::Close(
                imbo_proto::frames::CLOSE_REPLACED,
                "replaced",
            ));
        }
        (id, rx)
    }

    /// Remove the connection if it is still the registered one.
    pub fn unregister(&self, device: Uuid, conn_id: u64) -> bool {
        let mut map = self.conns.lock().unwrap();
        if map.get(&device).is_some_and(|c| c.id == conn_id) {
            map.remove(&device);
            true
        } else {
            false
        }
    }

    pub fn is_current(&self, device: Uuid, conn_id: u64) -> bool {
        self.conns
            .lock()
            .unwrap()
            .get(&device)
            .is_some_and(|c| c.id == conn_id)
    }

    pub fn connected(&self, device: Uuid) -> bool {
        self.conns.lock().unwrap().contains_key(&device)
    }

    pub fn send(&self, device: Uuid, frame: ServerFrame) -> bool {
        match self.conns.lock().unwrap().get(&device) {
            Some(c) => c.tx.send(Outbound::Frame(frame)).is_ok(),
            None => false,
        }
    }

    /// Send to every connected device of an account; returns how many got it.
    pub fn send_account(&self, account_id: i32, frame: &ServerFrame) -> usize {
        self.conns
            .lock()
            .unwrap()
            .values()
            .filter(|c| c.account_id == account_id)
            .filter(|c| c.tx.send(Outbound::Frame(frame.clone())).is_ok())
            .count()
    }

    pub fn close(&self, device: Uuid, code: u16, reason: &'static str) {
        if let Some(c) = self.conns.lock().unwrap().get(&device) {
            let _ = c.tx.send(Outbound::Close(code, reason));
        }
    }

    pub fn count(&self) -> usize {
        self.conns.lock().unwrap().len()
    }
}
