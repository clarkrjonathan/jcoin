//! The shared P2P runtime handle: the peer registry, the address book, gossip
//! fan-out, and the dialer.
//!
//! `Network` is cloned into every connection task and into the REST state. It
//! tracks connected peers by a local, ephemeral `PeerId` (not a protocol node
//! identifier), remembers learned peer addresses, and can dial a learned
//! `Direct` endpoint to grow the mesh.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tokio::net::TcpStream;
use tokio::sync::mpsc;

use crate::node::NodeRequest;
use crate::Transaction;

use super::addr::Endpoint;
use super::message::Message;

/// Local, per-process identifier for a connected peer. Assigned on connect and
/// forgotten on disconnect. It never appears on the wire.
pub type PeerId = u64;

/// Most addresses to return in one `ADDR` reply.
const ADDR_SAMPLE: usize = 64;

struct Inner {
    peers: Mutex<HashMap<PeerId, mpsc::Sender<Message>>>,
    next_id: AtomicU64,
    /// Learned peer addresses.
    book: Mutex<HashSet<Endpoint>>,
    /// Direct addresses already dialed, to avoid hammering the same peer.
    attempted: Mutex<HashSet<SocketAddr>>,
    /// This node's own reachable endpoints, advertised to peers.
    advertised: Vec<Endpoint>,
    /// This node's P2P listen port, placed in the outgoing `HELLO`.
    listen_port: u16,
    /// Channel into the node core.
    node: mpsc::Sender<NodeRequest>,
    /// Soft cap on the number of peers before the dialer stops reaching out.
    max_peers: usize,
}

#[derive(Clone)]
pub struct Network {
    inner: Arc<Inner>,
}

impl Network {
    pub fn new(
        node: mpsc::Sender<NodeRequest>,
        listen_port: u16,
        advertised: Vec<Endpoint>,
        max_peers: usize,
    ) -> Self {
        Network {
            inner: Arc::new(Inner {
                peers: Mutex::new(HashMap::new()),
                next_id: AtomicU64::new(0),
                book: Mutex::new(HashSet::new()),
                attempted: Mutex::new(HashSet::new()),
                advertised,
                listen_port,
                node,
                max_peers,
            }),
        }
    }

    pub fn node(&self) -> &mpsc::Sender<NodeRequest> {
        &self.inner.node
    }

    pub fn listen_port(&self) -> u16 {
        self.inner.listen_port
    }

    pub fn advertised(&self) -> &[Endpoint] {
        &self.inner.advertised
    }

    /// Register a peer's outbound channel and return its new id.
    pub fn register(&self, sender: mpsc::Sender<Message>) -> PeerId {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        self.inner.peers.lock().unwrap().insert(id, sender);
        id
    }

    /// Drop a peer from the registry.
    pub fn remove(&self, id: PeerId) {
        self.inner.peers.lock().unwrap().remove(&id);
    }

    pub fn peer_count(&self) -> usize {
        self.inner.peers.lock().unwrap().len()
    }

    /// Send a transaction to every peer except `except`. Non-blocking: a full
    /// peer queue drops the message for that peer (recovered via other gossip).
    pub fn broadcast_transaction(&self, tx: &Transaction, except: Option<PeerId>) {
        let peers = self.inner.peers.lock().unwrap();
        for (id, sender) in peers.iter() {
            if Some(*id) == except {
                continue;
            }
            let _ = sender.try_send(Message::NewTransaction(tx.clone()));
        }
    }

    /// Addresses to share in an `ADDR`: this node's own endpoints plus a sample
    /// of the book.
    pub fn known_addrs(&self) -> Vec<Endpoint> {
        let mut out = self.inner.advertised.clone();
        let book = self.inner.book.lock().unwrap();
        for endpoint in book.iter().take(ADDR_SAMPLE) {
            out.push(endpoint.clone());
        }
        out
    }

    /// Add endpoints to the book, returning the ones not already known. This
    /// node's own advertised endpoints are never stored.
    pub fn learn(&self, endpoints: Vec<Endpoint>) -> Vec<Endpoint> {
        let mut book = self.inner.book.lock().unwrap();
        let mut fresh = Vec::new();
        for endpoint in endpoints {
            if self.inner.advertised.contains(&endpoint) {
                continue;
            }
            if book.insert(endpoint.clone()) {
                fresh.push(endpoint);
            }
        }
        fresh
    }

    /// Dial newly-learned `Direct` endpoints, within the peer cap, skipping any
    /// already attempted or belonging to this node. `Relay` endpoints are left
    /// to the relay transport.
    pub fn maybe_dial(&self, endpoints: Vec<Endpoint>) {
        for endpoint in endpoints {
            let Endpoint::Direct(addr) = endpoint else {
                continue;
            };
            if self.peer_count() >= self.inner.max_peers {
                break;
            }
            if self.inner.advertised.contains(&Endpoint::Direct(addr)) {
                continue;
            }
            {
                let mut attempted = self.inner.attempted.lock().unwrap();
                if !attempted.insert(addr) {
                    continue; // already tried
                }
            }
            let this = self.clone();
            tokio::spawn(this.dial(addr));
        }
    }

    /// Connect to a direct address and run it as a peer. One-shot: on
    /// disconnect the address is cleared from `attempted` so it can be redialed
    /// if learned again.
    async fn dial(self, addr: SocketAddr) {
        match TcpStream::connect(addr).await {
            Ok(stream) => {
                super::peer::run_peer(stream, self.clone(), format!("{addr} (auto)")).await;
            }
            Err(e) => eprintln!("[p2p] auto-dial {addr} failed: {e}"),
        }
        self.inner.attempted.lock().unwrap().remove(&addr);
    }
}
