//! The shared P2P runtime handle: the peer registry, the address book, gossip
//! fan-out, and dial requests.
//!
//! `Network` is cloned into every connection task and into the REST state. It
//! tracks protocol sessions (one per `run_peer`) by a local, ephemeral
//! `PeerId` (a counter, not the libp2p peer id), remembers learned peer
//! addresses, and asks the libp2p host to dial newly learned ones.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use libp2p::multiaddr::Protocol;
use libp2p::Multiaddr;
use tokio::sync::mpsc;

use crate::block::Block;
use crate::node::NodeRequest;
use crate::Transaction;

use super::message::Message;

/// Whether an address is worth storing and passing on. A direct loopback or
/// link-local address is only reachable on the machine that sent it, so another
/// node can never dial it; keeping it just fills the book and the log with dials
/// that always fail. A relayed address (`/p2p-circuit`) is reached through the
/// relay, not by dialing its IP, so its IP is not judged here. Everything else
/// (including ordinary LAN addresses) is kept.
pub fn routable(addr: &Multiaddr) -> bool {
    let mut host_unreachable = false;
    for proto in addr.iter() {
        match proto {
            // A circuit address is dialed through the relay; its IP is the
            // relay's and is handled when the relay itself is reached.
            Protocol::P2pCircuit => return true,
            Protocol::Ip4(ip) => {
                // 127.0.0.0/8 loopback, 169.254.0.0/16 link-local.
                host_unreachable = ip.is_loopback() || ip.is_link_local();
            }
            Protocol::Ip6(ip) => {
                // ::1 loopback, fe80::/10 link-local.
                host_unreachable = ip.is_loopback() || (ip.segments()[0] & 0xffc0) == 0xfe80;
            }
            _ => {}
        }
    }
    !host_unreachable
}

/// Local, per-process identifier for one protocol session with a peer.
/// Assigned when `run_peer` starts and forgotten when it ends. It never
/// appears on the wire.
pub type PeerId = u64;

/// Most addresses to return in one `ADDR` reply.
const ADDR_SAMPLE: usize = 64;

/// Most entries read from one received `ADDR` message; the rest are ignored.
/// A legitimate reply holds at most `ADDR_SAMPLE` plus a few advertised
/// addresses, so this only limits abuse.
pub const MAX_ADDR_PER_MESSAGE: usize = 256;

/// Most addresses kept in the address book. When it is full, a new address
/// replaces a random existing one, so a single peer cannot fill the book once
/// and lock out every later address.
pub const MAX_BOOK: usize = 1_000;

/// The libp2p peer id at the end of an address (`.../p2p/<id>`), if any.
pub fn peer_of(addr: &Multiaddr) -> Option<libp2p::PeerId> {
    match addr.iter().last() {
        Some(Protocol::P2p(id)) => Some(id),
        _ => None,
    }
}

/// One open libp2p connection, for diagnostics (`GET /peers`).
#[derive(Debug, Clone)]
pub struct ConnectionInfo {
    /// The remote node's libp2p peer id.
    pub peer: String,
    /// The remote address of the connection.
    pub address: String,
    /// This node dialed it (otherwise the remote node did).
    pub outbound: bool,
    /// The connection runs through a relay circuit, not directly.
    pub relayed: bool,
    /// The remote node is this node's relay.
    pub is_relay: bool,
    /// A JeffyCoin protocol session runs with this peer (blocks and
    /// transactions are exchanged). A connection without one is only transport.
    pub session: bool,
    /// When the connection opened (Unix seconds).
    pub since: u64,
    /// The friendly label the peer sent after the handshake (what its user
    /// named the computer), if any. Filled in by `connections`.
    pub label: Option<String>,
}

struct Inner {
    peers: Mutex<HashMap<PeerId, mpsc::Sender<Message>>>,
    /// Log labels of the protocol sessions; each starts with the peer id.
    session_labels: Mutex<HashMap<PeerId, String>>,
    /// Friendly device labels peers sent, by libp2p peer id string.
    peer_labels: Mutex<HashMap<String, String>>,
    /// This node's own friendly label, sent to peers after the handshake.
    own_label: Mutex<Option<String>>,
    /// Open connections by libp2p connection id.
    connections: Mutex<HashMap<String, ConnectionInfo>>,
    next_id: AtomicU64,
    /// Learned peer addresses.
    book: Mutex<HashSet<Multiaddr>>,
    /// Addresses already handed to the host to dial, to avoid hammering the
    /// same peer. Cleared by the host when a dial fails or a peer disconnects.
    attempted: Mutex<HashSet<Multiaddr>>,
    /// Dials requested but not yet connected or failed. They count toward
    /// `max_peers`, so one `ADDR` cannot start thousands of dials.
    dialing: AtomicUsize,
    /// This node's own reachable addresses (with `/p2p/<own id>`), advertised
    /// to peers. Kept current by the libp2p host.
    advertised: Mutex<Vec<Multiaddr>>,
    /// libp2p peers with an open connection, kept current by the host.
    connected: Mutex<HashSet<libp2p::PeerId>>,
    /// This node's P2P listen port, placed in the outgoing `HELLO`.
    listen_port: u16,
    /// Channel into the node core.
    node: mpsc::Sender<NodeRequest>,
    /// Soft cap on the number of peers before the dialer stops reaching out.
    max_peers: usize,
    /// Dial requests to the libp2p host, once it is running.
    dialer: Mutex<Option<mpsc::Sender<Multiaddr>>>,
}

#[derive(Clone)]
pub struct Network {
    inner: Arc<Inner>,
}

impl Network {
    pub fn new(
        node: mpsc::Sender<NodeRequest>,
        listen_port: u16,
        advertised: Vec<Multiaddr>,
        max_peers: usize,
    ) -> Self {
        Network {
            inner: Arc::new(Inner {
                peers: Mutex::new(HashMap::new()),
                session_labels: Mutex::new(HashMap::new()),
                peer_labels: Mutex::new(HashMap::new()),
                own_label: Mutex::new(None),
                connections: Mutex::new(HashMap::new()),
                next_id: AtomicU64::new(0),
                book: Mutex::new(HashSet::new()),
                attempted: Mutex::new(HashSet::new()),
                dialing: AtomicUsize::new(0),
                advertised: Mutex::new(advertised),
                connected: Mutex::new(HashSet::new()),
                listen_port,
                node,
                max_peers,
                dialer: Mutex::new(None),
            }),
        }
    }

    pub fn node(&self) -> &mpsc::Sender<NodeRequest> {
        &self.inner.node
    }

    pub fn listen_port(&self) -> u16 {
        self.inner.listen_port
    }

    pub fn advertised(&self) -> Vec<Multiaddr> {
        self.inner.advertised.lock().unwrap().clone()
    }

    /// Add one of this node's own reachable addresses (the host calls this for
    /// each listen address and for a relayed address once a reservation holds).
    pub fn add_advertised(&self, addr: Multiaddr) {
        let mut advertised = self.inner.advertised.lock().unwrap();
        if !advertised.contains(&addr) {
            advertised.push(addr);
        }
    }

    /// Remove one of this node's addresses (for example, an expired listener).
    pub fn remove_advertised(&self, addr: &Multiaddr) {
        self.inner.advertised.lock().unwrap().retain(|a| a != addr);
    }

    /// Register the libp2p host's dial channel. Learned addresses are sent here.
    pub fn set_dialer(&self, dialer: mpsc::Sender<Multiaddr>) {
        *self.inner.dialer.lock().unwrap() = Some(dialer);
    }

    /// Register a protocol session's outbound channel and return its new id.
    pub fn register(&self, sender: mpsc::Sender<Message>) -> PeerId {
        let id = self.inner.next_id.fetch_add(1, Ordering::Relaxed);
        self.inner.peers.lock().unwrap().insert(id, sender);
        id
    }

    /// Drop a protocol session from the registry.
    pub fn remove(&self, id: PeerId) {
        self.inner.peers.lock().unwrap().remove(&id);
        self.inner.session_labels.lock().unwrap().remove(&id);
    }

    /// Set this node's own friendly label, sent to peers after the handshake.
    /// An empty label means none is sent.
    pub fn set_own_label(&self, label: Option<String>) {
        *self.inner.own_label.lock().unwrap() =
            label.filter(|l| !l.is_empty());
    }

    /// This node's own friendly label, if set.
    pub fn own_label(&self) -> Option<String> {
        self.inner.own_label.lock().unwrap().clone()
    }

    /// Record the friendly label a peer sent, by its libp2p peer id string.
    /// Shown next to the peer id in the connections list.
    pub fn set_peer_label(&self, peer: &str, label: &str) {
        let mut labels = self.inner.peer_labels.lock().unwrap();
        if label.is_empty() {
            labels.remove(peer);
        } else {
            labels.insert(peer.to_string(), label.to_string());
        }
    }

    /// Record the log label of session `id` (it starts with the peer id), so
    /// `connections` can tell which connections carry a session.
    pub fn label_session(&self, id: PeerId, label: &str) {
        self.inner
            .session_labels
            .lock()
            .unwrap()
            .insert(id, label.to_string());
    }

    /// Record an open connection under its libp2p connection id.
    pub fn connection_opened(&self, id: String, info: ConnectionInfo) {
        self.inner.connections.lock().unwrap().insert(id, info);
    }

    /// Forget a closed connection.
    pub fn connection_closed(&self, id: &str) {
        self.inner.connections.lock().unwrap().remove(id);
    }

    /// The open connections, oldest first, with `session` filled in.
    pub fn connections(&self) -> Vec<ConnectionInfo> {
        let labels = self.inner.session_labels.lock().unwrap();
        let friendly = self.inner.peer_labels.lock().unwrap();
        let mut list: Vec<ConnectionInfo> = self
            .inner
            .connections
            .lock()
            .unwrap()
            .values()
            .cloned()
            .map(|mut c| {
                c.session = labels.values().any(|label| label.starts_with(&c.peer));
                c.label = friendly.get(&c.peer).cloned();
                c
            })
            .collect();
        list.sort_by(|a, b| a.since.cmp(&b.since).then(a.peer.cmp(&b.peer)));
        list
    }

    /// Number of protocol sessions (peers we gossip with).
    pub fn peer_count(&self) -> usize {
        self.inner.peers.lock().unwrap().len()
    }

    /// Record that the host now has (or no longer has) a connection to `peer`.
    /// On disconnect, its addresses may be dialed again when learned.
    pub fn set_connected(&self, peer: libp2p::PeerId, connected: bool) {
        if connected {
            self.inner.connected.lock().unwrap().insert(peer);
        } else {
            self.inner.connected.lock().unwrap().remove(&peer);
            self.inner
                .attempted
                .lock()
                .unwrap()
                .retain(|a| peer_of(a) != Some(peer));
        }
    }

    pub fn is_connected(&self, peer: &libp2p::PeerId) -> bool {
        self.inner.connected.lock().unwrap().contains(peer)
    }

    /// The host reports that a requested dial finished. A failed address may
    /// be dialed again when learned again.
    pub fn dial_finished(&self, addr: &Multiaddr, succeeded: bool) {
        let _ = self
            .inner
            .dialing
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_sub(1));
        if !succeeded {
            self.inner.attempted.lock().unwrap().remove(addr);
        }
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

    /// Addresses to share in an `ADDR`: this node's own addresses plus a
    /// sample of the book.
    pub fn known_addrs(&self) -> Vec<Multiaddr> {
        let mut out = self.inner.advertised.lock().unwrap().clone();
        let book = self.inner.book.lock().unwrap();
        for addr in book.iter().take(ADDR_SAMPLE) {
            out.push(addr.clone());
        }
        out
    }

    /// Add addresses to the book, returning the ones not already known. This
    /// node's own advertised addresses are never stored. At most
    /// `MAX_ADDR_PER_MESSAGE` entries are read, and the book never grows past
    /// `MAX_BOOK` (a random entry is evicted to make room).
    pub fn learn(&self, addrs: Vec<Multiaddr>) -> Vec<Multiaddr> {
        use rand::Rng;

        let advertised = self.inner.advertised.lock().unwrap().clone();
        let mut book = self.inner.book.lock().unwrap();
        let mut rng = rand::thread_rng();
        let mut fresh = Vec::new();
        for addr in addrs.into_iter().take(MAX_ADDR_PER_MESSAGE) {
            // Drop loopback and link-local addresses: no other node can reach
            // them, so storing or dialing them only makes noise.
            if !routable(&addr) {
                continue;
            }
            if advertised.contains(&addr) || book.contains(&addr) {
                continue;
            }
            if book.len() >= MAX_BOOK {
                let victim = book.iter().nth(rng.gen_range(0..book.len())).cloned();
                if let Some(victim) = victim {
                    book.remove(&victim);
                }
            }
            book.insert(addr.clone());
            fresh.push(addr);
        }
        fresh
    }

    /// Number of addresses in the address book.
    pub fn book_len(&self) -> usize {
        self.inner.book.lock().unwrap().len()
    }

    /// Number of dials in progress.
    pub fn dials_in_flight(&self) -> usize {
        self.inner.dialing.load(Ordering::Relaxed)
    }

    /// Ask the host to dial newly learned addresses, within the peer cap,
    /// skipping this node's own addresses, peers already connected, and
    /// addresses already attempted.
    pub fn maybe_dial(&self, addrs: Vec<Multiaddr>) {
        let Some(dialer) = self.inner.dialer.lock().unwrap().clone() else {
            return;
        };
        let advertised = self.inner.advertised.lock().unwrap().clone();
        let own: HashSet<libp2p::PeerId> = advertised.iter().filter_map(peer_of).collect();
        for addr in addrs {
            let in_flight = self.inner.dialing.load(Ordering::Relaxed);
            if self.peer_count() + in_flight >= self.inner.max_peers {
                return;
            }
            if advertised.contains(&addr) {
                continue;
            }
            if let Some(peer) = peer_of(&addr) {
                if own.contains(&peer) || self.is_connected(&peer) {
                    continue;
                }
            }
            if !self.inner.attempted.lock().unwrap().insert(addr.clone()) {
                continue; // already tried
            }
            self.inner.dialing.fetch_add(1, Ordering::Relaxed);
            if dialer.try_send(addr.clone()).is_err() {
                self.dial_finished(&addr, false);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(i: u32) -> Multiaddr {
        let ip = std::net::Ipv4Addr::from(0x0a00_0000 + i); // 10.x.x.x
        format!("/ip4/{ip}/tcp/8333").parse().unwrap()
    }

    fn network(max_peers: usize) -> Network {
        let (node, _rx) = mpsc::channel(1);
        Network::new(node, 8333, Vec::new(), max_peers)
    }

    #[test]
    fn loopback_and_link_local_are_not_routable_or_learned() {
        let parse = |s: &str| s.parse::<Multiaddr>().unwrap();
        assert!(!routable(&parse("/ip4/127.0.0.1/tcp/8333")));
        assert!(!routable(&parse("/ip4/169.254.1.2/tcp/8333")));
        assert!(!routable(&parse("/ip6/::1/tcp/8333")));
        assert!(!routable(&parse("/ip6/fe80::1/tcp/8333")));
        // Ordinary LAN and public addresses are kept.
        assert!(routable(&parse("/ip4/192.168.0.5/tcp/8333")));
        assert!(routable(&parse("/ip4/10.0.0.9/tcp/8333")));
        assert!(routable(&parse("/ip4/3.14.123.27/tcp/8333")));

        let net = network(8);
        let fresh = net.learn(vec![
            parse("/ip4/127.0.0.1/tcp/8333"),
            parse("/ip4/169.254.0.7/tcp/8333"),
            parse("/ip4/192.168.0.5/tcp/8333"),
        ]);
        assert_eq!(fresh, vec![parse("/ip4/192.168.0.5/tcp/8333")]);
        assert_eq!(net.book_len(), 1);
    }

    // S5: one oversized ADDR is read only up to MAX_ADDR_PER_MESSAGE, and the
    // book never grows past MAX_BOOK however many messages arrive.
    #[test]
    fn address_book_is_bounded() {
        let net = network(8);
        let fresh = net.learn((0..60_000).map(addr).collect());
        assert_eq!(fresh.len(), MAX_ADDR_PER_MESSAGE);
        for batch in 1..20u32 {
            let start = batch * 10_000;
            net.learn((start..start + 1_000).map(addr).collect());
        }
        assert_eq!(net.book_len(), MAX_BOOK);
    }

    #[test]
    fn known_and_own_addresses_are_not_fresh() {
        let (node, _rx) = mpsc::channel(1);
        let net = Network::new(node, 8333, vec![addr(1)], 8);
        let fresh = net.learn(vec![addr(1), addr(2), addr(2)]);
        assert_eq!(fresh, vec![addr(2)]);
        assert!(net.learn(vec![addr(2)]).is_empty());
    }

    // S5: in-flight dials count toward max_peers, so learning many addresses
    // requests at most max_peers dials.
    #[tokio::test]
    async fn dials_in_flight_are_capped_by_max_peers() {
        let net = network(4);
        let (dial_tx, mut dial_rx) = mpsc::channel(1_000);
        net.set_dialer(dial_tx);
        let fresh = net.learn((0..200).map(addr).collect());
        net.maybe_dial(fresh);
        assert_eq!(net.dials_in_flight(), 4);
        let mut requested = 0;
        while dial_rx.try_recv().is_ok() {
            requested += 1;
        }
        assert_eq!(requested, 4);
        // A failed dial frees its slot, and its address may be tried again.
        net.dial_finished(&addr(0), false);
        assert_eq!(net.dials_in_flight(), 3);
        net.maybe_dial(vec![addr(0)]);
        assert_eq!(net.dials_in_flight(), 4);
    }

    #[test]
    fn a_peers_label_shows_up_in_its_connection() {
        let net = network(8);
        net.connection_opened(
            "conn-1".to_string(),
            ConnectionInfo {
                peer: "12D3KooWABCDEF".to_string(),
                address: "/ip4/1.2.3.4/tcp/8333".to_string(),
                outbound: true,
                relayed: false,
                is_relay: false,
                session: false,
                since: 1,
                label: None,
            },
        );
        // Before the peer sends its label, there is none.
        assert_eq!(net.connections()[0].label, None);
        net.set_peer_label("12D3KooWABCDEF", "Noahs PC");
        assert_eq!(net.connections()[0].label.as_deref(), Some("Noahs PC"));
        // Our own label round-trips, and an empty one is dropped.
        net.set_own_label(Some("Jons Desktop".to_string()));
        assert_eq!(net.own_label().as_deref(), Some("Jons Desktop"));
        net.set_own_label(Some(String::new()));
        assert_eq!(net.own_label(), None);
    }

    #[test]
    fn peer_of_reads_the_trailing_peer_id() {
        let id = libp2p::PeerId::random();
        let with: Multiaddr = format!("/ip4/127.0.0.1/tcp/1/p2p/{id}").parse().unwrap();
        assert_eq!(peer_of(&with), Some(id));
        assert_eq!(peer_of(&addr(1)), None);
    }
}
