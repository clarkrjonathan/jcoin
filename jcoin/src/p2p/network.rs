//! The peer registry and gossip fan-out.
//!
//! `Network` tracks connected peers by a local, ephemeral `PeerId` (not a
//! protocol node identifier) and holds one outbound channel per peer. Gossip
//! sends a message to every peer except an optional source, which is how a
//! relayed transaction avoids being echoed back to where it came from.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;

use crate::Transaction;

use super::message::Message;

/// Local, per-process identifier for a connected peer. Assigned on connect and
/// forgotten on disconnect. It never appears on the wire.
pub type PeerId = u64;

#[derive(Clone, Default)]
pub struct Network {
    peers: Arc<Mutex<HashMap<PeerId, mpsc::Sender<Message>>>>,
    next_id: Arc<AtomicU64>,
}

impl Network {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a peer's outbound channel and return its new id.
    pub fn register(&self, sender: mpsc::Sender<Message>) -> PeerId {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.peers.lock().unwrap().insert(id, sender);
        id
    }

    /// Drop a peer from the registry.
    pub fn remove(&self, id: PeerId) {
        self.peers.lock().unwrap().remove(&id);
    }

    /// Number of currently connected peers.
    pub fn peer_count(&self) -> usize {
        self.peers.lock().unwrap().len()
    }

    /// Send a transaction to every peer except `except`.
    ///
    /// Non-blocking: if a peer's outbound queue is full, the message is dropped
    /// for that peer. At this scale queues do not fill; a dropped gossip message
    /// is recovered from another peer or a later announcement.
    pub fn broadcast_transaction(&self, tx: &Transaction, except: Option<PeerId>) {
        let peers = self.peers.lock().unwrap();
        for (id, sender) in peers.iter() {
            if Some(*id) == except {
                continue;
            }
            let _ = sender.try_send(Message::NewTransaction(tx.clone()));
        }
    }
}
