//! The shared P2P runtime handle: the peer registry, the address book, gossip
//! fan-out, and the dialer.
//!
//! `Network` is cloned into every connection task and into the REST state. It
//! tracks connected peers by a local, ephemeral `PeerId` (not a protocol node
//! identifier), remembers learned peer addresses, and can dial a learned
//! `Direct` endpoint to grow the mesh.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tokio::net::TcpStream;
use tokio::sync::mpsc;

use crate::block::Block;
use crate::node::NodeRequest;
use crate::Transaction;

use super::addr::{Endpoint, Handle};
use super::message::Message;

/// Local, per-process identifier for a connected peer. Assigned on connect and
/// forgotten on disconnect. It never appears on the wire.
pub type PeerId = u64;

/// Most addresses to return in one `ADDR` reply.
const ADDR_SAMPLE: usize = 64;

/// Most entries read from one received `ADDR` message; the rest are ignored.
/// A legitimate reply holds at most `ADDR_SAMPLE` plus a few advertised
/// endpoints, so this only limits abuse.
pub const MAX_ADDR_PER_MESSAGE: usize = 256;

/// Most endpoints kept in the address book. When it is full, a new endpoint
/// replaces a random existing one, so a single peer cannot fill the book once
/// and lock out every later address.
pub const MAX_BOOK: usize = 1_000;

struct Inner {
    peers: Mutex<HashMap<PeerId, mpsc::Sender<Message>>>,
    next_id: AtomicU64,
    /// Learned peer addresses.
    book: Mutex<HashSet<Endpoint>>,
    /// Direct addresses already dialed, to avoid hammering the same peer.
    attempted: Mutex<HashSet<SocketAddr>>,
    /// Outbound dials started but not yet connected or failed. They count
    /// toward `max_peers`, so one `ADDR` cannot start thousands of dials.
    dialing: AtomicUsize,
    /// This node's own reachable endpoints, advertised to peers. Mutable so the
    /// relay client can add a `Relay` endpoint once it has a handle.
    advertised: Mutex<Vec<Endpoint>>,
    /// This node's P2P listen port, placed in the outgoing `HELLO`.
    listen_port: u16,
    /// Channel into the node core.
    node: mpsc::Sender<NodeRequest>,
    /// Soft cap on the number of peers before the dialer stops reaching out.
    max_peers: usize,
    /// When a relay client is running, learned `Relay` endpoints are opened
    /// through it by sending the handle here.
    relay_open: Mutex<Option<mpsc::Sender<Handle>>>,
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
                dialing: AtomicUsize::new(0),
                advertised: Mutex::new(advertised),
                listen_port,
                node,
                max_peers,
                relay_open: Mutex::new(None),
            }),
        }
    }

    pub fn node(&self) -> &mpsc::Sender<NodeRequest> {
        &self.inner.node
    }

    pub fn listen_port(&self) -> u16 {
        self.inner.listen_port
    }

    pub fn advertised(&self) -> Vec<Endpoint> {
        self.inner.advertised.lock().unwrap().clone()
    }

    /// Add one of this node's own reachable endpoints (used by the relay client
    /// once it has a handle).
    pub fn add_advertised(&self, endpoint: Endpoint) {
        let mut advertised = self.inner.advertised.lock().unwrap();
        if !advertised.contains(&endpoint) {
            advertised.push(endpoint);
        }
    }

    /// Register the relay client's open channel. Learned `Relay` endpoints are
    /// routed here to be opened.
    pub fn set_relay_opener(&self, opener: mpsc::Sender<Handle>) {
        *self.inner.relay_open.lock().unwrap() = Some(opener);
    }

    /// Clear the relay opener when the relay client stops.
    pub fn clear_relay_opener(&self) {
        *self.inner.relay_open.lock().unwrap() = None;
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

    /// Send a block to every peer except `except`.
    pub fn broadcast_block(&self, block: &Block, except: Option<PeerId>) {
        let peers = self.inner.peers.lock().unwrap();
        for (id, sender) in peers.iter() {
            if Some(*id) == except {
                continue;
            }
            let _ = sender.try_send(Message::NewBlock(block.clone()));
        }
    }

    /// Addresses to share in an `ADDR`: this node's own endpoints plus a sample
    /// of the book.
    pub fn known_addrs(&self) -> Vec<Endpoint> {
        let mut out = self.inner.advertised.lock().unwrap().clone();
        let book = self.inner.book.lock().unwrap();
        for endpoint in book.iter().take(ADDR_SAMPLE) {
            out.push(endpoint.clone());
        }
        out
    }

    /// Handles in the book reachable through the given relay. Used by the relay
    /// client at registration to open channels to peers already known.
    pub fn relay_handles(&self, relay: SocketAddr) -> Vec<Handle> {
        self.inner
            .book
            .lock()
            .unwrap()
            .iter()
            .filter_map(|endpoint| match endpoint {
                Endpoint::Relay { relay: r, handle } if *r == relay => Some(*handle),
                _ => None,
            })
            .collect()
    }

    /// Add endpoints to the book, returning the ones not already known. This
    /// node's own advertised endpoints are never stored. At most
    /// `MAX_ADDR_PER_MESSAGE` entries are read, and the book never grows past
    /// `MAX_BOOK` (a random entry is evicted to make room).
    pub fn learn(&self, endpoints: Vec<Endpoint>) -> Vec<Endpoint> {
        use rand::Rng;

        let advertised = self.inner.advertised.lock().unwrap().clone();
        let mut book = self.inner.book.lock().unwrap();
        let mut rng = rand::thread_rng();
        let mut fresh = Vec::new();
        for endpoint in endpoints.into_iter().take(MAX_ADDR_PER_MESSAGE) {
            if advertised.contains(&endpoint) || book.contains(&endpoint) {
                continue;
            }
            if book.len() >= MAX_BOOK {
                let victim = book.iter().nth(rng.gen_range(0..book.len())).cloned();
                if let Some(victim) = victim {
                    book.remove(&victim);
                }
            }
            book.insert(endpoint.clone());
            fresh.push(endpoint);
        }
        fresh
    }

    /// Number of endpoints in the address book.
    pub fn book_len(&self) -> usize {
        self.inner.book.lock().unwrap().len()
    }

    /// Number of outbound dials in progress.
    pub fn dials_in_flight(&self) -> usize {
        self.inner.dialing.load(Ordering::Relaxed)
    }

    /// Act on newly-learned endpoints. A `Direct` endpoint is dialed within the
    /// peer cap, skipping any already attempted or belonging to this node. A
    /// `Relay` endpoint is handed to the relay client to open, if one is running.
    pub fn maybe_dial(&self, endpoints: Vec<Endpoint>) {
        let advertised = self.inner.advertised.lock().unwrap().clone();
        for endpoint in endpoints {
            match endpoint {
                Endpoint::Direct(addr) => {
                    let in_flight = self.inner.dialing.load(Ordering::Relaxed);
                    if self.peer_count() + in_flight >= self.inner.max_peers {
                        continue;
                    }
                    if advertised.contains(&Endpoint::Direct(addr)) {
                        continue;
                    }
                    {
                        let mut attempted = self.inner.attempted.lock().unwrap();
                        if !attempted.insert(addr) {
                            continue; // already tried
                        }
                    }
                    self.inner.dialing.fetch_add(1, Ordering::Relaxed);
                    let this = self.clone();
                    tokio::spawn(this.dial(addr));
                }
                Endpoint::Relay { handle, .. } => {
                    let opener = self.inner.relay_open.lock().unwrap().clone();
                    if let Some(tx) = opener {
                        let _ = tx.try_send(handle);
                    }
                }
            }
        }
    }

    /// Connect to a direct address and run it as a peer. One-shot: on
    /// disconnect the address is cleared from `attempted` so it can be redialed
    /// if learned again.
    async fn dial(self, addr: SocketAddr) {
        let connected = TcpStream::connect(addr).await;
        // The dial is no longer in flight; a connected peer is counted by the
        // registry once its handshake completes.
        self.inner.dialing.fetch_sub(1, Ordering::Relaxed);
        match connected {
            Ok(stream) => {
                super::peer::run_peer(stream, self.clone(), format!("{addr} (auto)")).await;
            }
            Err(e) => eprintln!("[p2p] auto-dial {addr} failed: {e}"),
        }
        self.inner.attempted.lock().unwrap().remove(&addr);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn direct(i: u32) -> Endpoint {
        let ip = std::net::Ipv4Addr::from(0x0a00_0000 + i); // 10.x.x.x
        Endpoint::Direct(SocketAddr::from((ip, 8333)))
    }

    fn network(max_peers: usize) -> Network {
        let (node, _rx) = mpsc::channel(1);
        Network::new(node, 8333, Vec::new(), max_peers)
    }

    // S5: one oversized ADDR is read only up to MAX_ADDR_PER_MESSAGE, and the
    // book never grows past MAX_BOOK however many messages arrive.
    #[test]
    fn address_book_is_bounded() {
        let net = network(8);
        let fresh = net.learn((0..60_000).map(direct).collect());
        assert_eq!(fresh.len(), MAX_ADDR_PER_MESSAGE);
        for batch in 1..20u32 {
            let start = batch * 10_000;
            net.learn((start..start + 1_000).map(direct).collect());
        }
        assert_eq!(net.book_len(), MAX_BOOK);
    }

    #[test]
    fn known_and_own_endpoints_are_not_fresh() {
        let (node, _rx) = mpsc::channel(1);
        let net = Network::new(node, 8333, vec![direct(1)], 8);
        let fresh = net.learn(vec![direct(1), direct(2), direct(2)]);
        assert_eq!(fresh, vec![direct(2)]);
        assert!(net.learn(vec![direct(2)]).is_empty());
    }

    // S5: in-flight dials count toward max_peers, so learning many endpoints
    // starts at most max_peers dials.
    #[tokio::test]
    async fn dials_in_flight_are_capped_by_max_peers() {
        let net = network(4);
        let fresh = net.learn((0..200).map(direct).collect());
        net.maybe_dial(fresh);
        assert!(net.dials_in_flight() <= 4, "{} dials", net.dials_in_flight());
    }
}
