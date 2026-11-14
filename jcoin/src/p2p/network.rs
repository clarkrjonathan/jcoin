//! The shared P2P runtime handle: the peer registry, the address book, gossip
//! fan-out, and dial requests.
//!
//! `Network` is cloned into every connection task and into the REST state. It
//! tracks protocol sessions (one per `run_peer`) by a local, ephemeral
//! `PeerId` (a counter, not the libp2p peer id), keeps the address book (saved
//! to `BOOK_FILE` when the node has a data folder), and asks the libp2p host to
//! dial newly learned addresses and, while below `max_peers`, random ones from
//! the book.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

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

/// The address book's file in the data folder: one line per address, the
/// Unix second it was last vouched for, a space, the multiaddr.
pub const BOOK_FILE: &str = "peers.txt";

/// An address not vouched for in this long is dropped from the book.
pub const BOOK_TTL_SECS: u64 = 7 * 24 * 3600;

/// An address from the book is dialed at most this often.
pub const REDIAL_AFTER: Duration = Duration::from_secs(600);

/// One address in the book.
#[derive(Clone, Debug)]
struct Entry {
    /// Unix seconds when the address was last vouched for: sent by the peer it
    /// names (in an `ADDR` or a rendezvous answer), or dialed successfully. An
    /// address passed on by another peer keeps the time it was first learned,
    /// so a dead address does not live on by being passed around.
    seen: u64,
    /// When this process last asked the host to dial it.
    tried: Option<Instant>,
}

fn now_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

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
    book: Mutex<HashMap<Multiaddr, Entry>>,
    /// Where the book is saved, if the node has a data folder.
    book_file: Mutex<Option<PathBuf>>,
    /// The book changed since it was last saved.
    book_dirty: AtomicBool,
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
                book: Mutex::new(HashMap::new()),
                book_file: Mutex::new(None),
                book_dirty: AtomicBool::new(false),
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

    /// The libp2p peer id of session `id`, read from its log label.
    pub fn session_peer(&self, id: PeerId) -> Option<libp2p::PeerId> {
        let labels = self.inner.session_labels.lock().unwrap();
        labels.get(&id)?.split(' ').next()?.parse().ok()
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

    /// Whether an open connection to `peer` runs directly, not through a relay.
    pub fn has_direct_connection(&self, peer: &libp2p::PeerId) -> bool {
        let peer = peer.to_string();
        self.inner
            .connections
            .lock()
            .unwrap()
            .values()
            .any(|c| c.peer == peer && !c.relayed)
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
    /// random sample of the book.
    pub fn known_addrs(&self) -> Vec<Multiaddr> {
        use rand::seq::IteratorRandom;

        let mut out = self.inner.advertised.lock().unwrap().clone();
        let book = self.inner.book.lock().unwrap();
        out.extend(book.keys().cloned().choose_multiple(&mut rand::thread_rng(), ADDR_SAMPLE));
        out
    }

    /// Add addresses passed on by another node to the book, returning the
    /// ones not already known. See `learn_from`.
    pub fn learn(&self, addrs: Vec<Multiaddr>) -> Vec<Multiaddr> {
        self.learn_from(addrs, None)
    }

    /// Add addresses to the book, returning the ones not already known. An
    /// address that names `source` (the peer that sent it) is vouched for:
    /// its time is set to now even if it was known. This node's own advertised
    /// addresses are never stored. At most `MAX_ADDR_PER_MESSAGE` entries are
    /// read, and the book never grows past `MAX_BOOK` (a random entry is
    /// evicted to make room).
    pub fn learn_from(&self, addrs: Vec<Multiaddr>, source: Option<libp2p::PeerId>) -> Vec<Multiaddr> {
        let advertised = self.inner.advertised.lock().unwrap().clone();
        let mut book = self.inner.book.lock().unwrap();
        let now = now_secs();
        let mut fresh = Vec::new();
        for addr in addrs.into_iter().take(MAX_ADDR_PER_MESSAGE) {
            // Drop loopback and link-local addresses: no other node can reach
            // them, so storing or dialing them only makes noise.
            if !routable(&addr) || advertised.contains(&addr) {
                continue;
            }
            let vouched = source.is_some() && peer_of(&addr) == source;
            if let Some(entry) = book.get_mut(&addr) {
                if vouched && entry.seen != now {
                    entry.seen = now;
                    self.inner.book_dirty.store(true, Ordering::Relaxed);
                }
                continue;
            }
            insert_bounded(&mut book, addr.clone(), Entry { seen: now, tried: None });
            self.inner.book_dirty.store(true, Ordering::Relaxed);
            fresh.push(addr);
        }
        fresh
    }

    /// A dial to `addr` succeeded: it is vouched for now.
    pub fn vouch(&self, addr: &Multiaddr) {
        if let Some(entry) = self.inner.book.lock().unwrap().get_mut(addr) {
            entry.seen = now_secs();
            self.inner.book_dirty.store(true, Ordering::Relaxed);
        }
    }

    /// Read the book saved in `path` and save to it from now on. Bad lines,
    /// unreachable addresses and expired entries are skipped. Returns the
    /// number of addresses read; a missing file reads as none.
    pub fn use_book_file(&self, path: PathBuf) -> std::io::Result<usize> {
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e),
        };
        let now = now_secs();
        let advertised = self.inner.advertised.lock().unwrap().clone();
        let mut book = self.inner.book.lock().unwrap();
        let mut read = 0;
        for line in text.lines() {
            let Some((seen, addr)) = line.trim().split_once(' ') else {
                continue;
            };
            let (Ok(seen), Ok(addr)) = (seen.parse::<u64>(), addr.trim().parse::<Multiaddr>()) else {
                continue;
            };
            if now.saturating_sub(seen) > BOOK_TTL_SECS || !routable(&addr) || advertised.contains(&addr) {
                continue;
            }
            let seen = seen.max(book.get(&addr).map_or(0, |e| e.seen));
            insert_bounded(&mut book, addr, Entry { seen, tried: None });
            read += 1;
        }
        *self.inner.book_file.lock().unwrap() = Some(path);
        Ok(read)
    }

    /// Write the book to its file if it changed since the last write.
    /// Returns whether it wrote.
    pub fn save_book(&self) -> std::io::Result<bool> {
        let Some(path) = self.inner.book_file.lock().unwrap().clone() else {
            return Ok(false);
        };
        if !self.inner.book_dirty.swap(false, Ordering::Relaxed) {
            return Ok(false);
        }
        let text: String = {
            let book = self.inner.book.lock().unwrap();
            let mut lines: Vec<(&Multiaddr, &Entry)> = book.iter().collect();
            lines.sort_by(|a, b| b.1.seen.cmp(&a.1.seen).then(a.0.cmp(b.0)));
            lines.iter().map(|(addr, e)| format!("{} {addr}\n", e.seen)).collect()
        };
        write_replacing(&path, &text).inspect_err(|_| {
            self.inner.book_dirty.store(true, Ordering::Relaxed);
        })?;
        Ok(true)
    }

    /// Drop addresses not vouched for in `BOOK_TTL_SECS`. Returns how many.
    pub fn expire_book(&self) -> usize {
        self.expire_book_at(now_secs())
    }

    fn expire_book_at(&self, now: u64) -> usize {
        let mut book = self.inner.book.lock().unwrap();
        let before = book.len();
        book.retain(|_, e| now.saturating_sub(e.seen) <= BOOK_TTL_SECS);
        let dropped = before - book.len();
        if dropped > 0 {
            self.inner.book_dirty.store(true, Ordering::Relaxed);
        }
        dropped
    }

    /// Dial from the book; none of it is dialed more than once per
    /// `REDIAL_AFTER`. First, the direct addresses of peers connected only
    /// through the relay (a plain dial reaches a peer with a public address,
    /// where a hole punch cannot; these need no free slot). Then, while below
    /// `max_peers`, addresses picked at random of peers not connected and not
    /// this node.
    pub fn dial_from_book(&self) {
        use rand::seq::IteratorRandom;

        let own: HashSet<libp2p::PeerId> =
            self.inner.advertised.lock().unwrap().iter().filter_map(peer_of).collect();
        let connected = self.inner.connected.lock().unwrap().clone();
        let due = |e: &Entry| e.tried.is_none_or(|t| t.elapsed() >= REDIAL_AFTER);
        let circuit = |a: &Multiaddr| a.iter().any(|p| p == Protocol::P2pCircuit);

        let upgrades: Vec<Multiaddr> = {
            let book = self.inner.book.lock().unwrap();
            book.iter()
                .filter(|(a, e)| due(e) && !circuit(a))
                .filter(|(a, _)| peer_of(a).is_some_and(|p| connected.contains(&p)))
                .map(|(a, _)| a.clone())
                .collect()
        };
        let upgrades: Vec<Multiaddr> = upgrades
            .into_iter()
            .filter(|a| peer_of(a).is_some_and(|p| !self.has_direct_connection(&p)))
            .collect();
        self.maybe_dial(upgrades);

        let in_flight = self.inner.dialing.load(Ordering::Relaxed);
        let slots = self.inner.max_peers.saturating_sub(self.peer_count() + in_flight);
        if slots == 0 {
            return;
        }
        let picked: Vec<Multiaddr> = {
            let book = self.inner.book.lock().unwrap();
            book.iter()
                .filter(|(_, e)| due(e))
                .filter(|(a, _)| peer_of(a).is_none_or(|p| !own.contains(&p) && !connected.contains(&p)))
                .map(|(a, _)| a.clone())
                .choose_multiple(&mut rand::thread_rng(), slots)
        };
        self.maybe_dial(picked);
    }

    #[cfg(test)]
    fn book_entry(&self, addr: &Multiaddr) -> Option<Entry> {
        self.inner.book.lock().unwrap().get(addr).cloned()
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
            if advertised.contains(&addr) {
                continue;
            }
            // A peer reached only through the relay is worth one direct try:
            // on the same LAN it usually works, and it takes the traffic off
            // the relay. It adds no session, so `max_peers` does not apply.
            let mut upgrade = false;
            if let Some(peer) = peer_of(&addr) {
                if own.contains(&peer) {
                    continue;
                }
                if self.is_connected(&peer) {
                    let circuit = addr.iter().any(|p| p == Protocol::P2pCircuit);
                    if circuit || self.has_direct_connection(&peer) {
                        continue;
                    }
                    upgrade = true;
                }
            }
            let in_flight = self.inner.dialing.load(Ordering::Relaxed);
            if !upgrade && self.peer_count() + in_flight >= self.inner.max_peers {
                continue;
            }
            if !self.inner.attempted.lock().unwrap().insert(addr.clone()) {
                continue; // already tried
            }
            if let Some(entry) = self.inner.book.lock().unwrap().get_mut(&addr) {
                entry.tried = Some(Instant::now());
            }
            self.inner.dialing.fetch_add(1, Ordering::Relaxed);
            if dialer.try_send(addr.clone()).is_err() {
                self.dial_finished(&addr, false);
            }
        }
    }
}

/// Insert into the book; when it is full, a random entry makes room.
fn insert_bounded(book: &mut HashMap<Multiaddr, Entry>, addr: Multiaddr, entry: Entry) {
    use rand::seq::IteratorRandom;

    if !book.contains_key(&addr) && book.len() >= MAX_BOOK {
        let victim = book.keys().choose(&mut rand::thread_rng()).cloned();
        if let Some(victim) = victim {
            book.remove(&victim);
        }
    }
    book.insert(addr, entry);
}

/// Write `text` to `path` through a temporary file, so a crash mid-write
/// leaves the old file whole.
fn write_replacing(path: &Path, text: &str) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
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

    /// A peer connected only through the relay gets one direct dial to a LAN
    /// address learned for it, even with every session slot full; a circuit
    /// address or a peer already reached directly gets none.
    #[tokio::test]
    async fn a_relayed_peer_is_dialed_directly_once() {
        let peer = libp2p::identity::Keypair::generate_ed25519().public().to_peer_id();
        let lan: Multiaddr = format!("/ip4/192.168.0.7/tcp/8333/p2p/{peer}").parse().unwrap();
        let circuit: Multiaddr =
            format!("/ip4/3.14.123.27/tcp/9000/p2p/{peer}/p2p-circuit/p2p/{peer}").parse().unwrap();
        let open = |net: &Network, id: &str, relayed: bool| {
            net.connection_opened(
                id.to_string(),
                ConnectionInfo {
                    peer: peer.to_string(),
                    address: String::new(),
                    outbound: true,
                    relayed,
                    is_relay: false,
                    session: false,
                    since: 1,
                    label: None,
                },
            )
        };

        let net = network(0); // no session slots: only an upgrade may dial
        let (dial_tx, mut dial_rx) = mpsc::channel(16);
        net.set_dialer(dial_tx);
        net.set_connected(peer, true);
        open(&net, "relayed", true);

        net.maybe_dial(vec![circuit.clone(), lan.clone()]);
        assert_eq!(dial_rx.try_recv().ok(), Some(lan.clone()));
        assert!(dial_rx.try_recv().is_err(), "the circuit address is not dialed");
        // Learned again while the dial is pending or done: not dialed again.
        net.maybe_dial(vec![lan.clone()]);
        assert!(dial_rx.try_recv().is_err());

        // Once a direct connection exists, a new LAN address is not dialed.
        let other: Multiaddr = format!("/ip4/192.168.0.8/tcp/8333/p2p/{peer}").parse().unwrap();
        open(&net, "direct", false);
        net.maybe_dial(vec![other]);
        assert!(dial_rx.try_recv().is_err());
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

    fn peer_addr(peer: libp2p::PeerId, i: u32) -> Multiaddr {
        addr(i).with(Protocol::P2p(peer))
    }

    fn temp_book(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("jcoin-book-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(BOOK_FILE)
    }

    /// The book is saved and read back; expired entries, bad lines and
    /// loopback addresses are not read; a second-hand address keeps its old
    /// time, and one its own peer sends is vouched for again.
    #[test]
    fn the_book_is_saved_read_back_and_vouched_for() {
        let path = temp_book("roundtrip");
        let (a, b) = (libp2p::PeerId::random(), libp2p::PeerId::random());
        let old = now_secs() - 3 * 24 * 3600;
        let stale = now_secs() - BOOK_TTL_SECS - 60;
        std::fs::write(
            &path,
            format!(
                "{old} {}\n{old} {}\n{stale} {}\nnot a line\n{old} /ip4/127.0.0.1/tcp/1\n",
                peer_addr(a, 1),
                peer_addr(b, 2),
                peer_addr(b, 3)
            ),
        )
        .unwrap();

        let net = network(8);
        assert_eq!(net.use_book_file(path.clone()).unwrap(), 2);
        assert_eq!(net.book_len(), 2);
        assert!(!net.save_book().unwrap(), "nothing changed, nothing written");

        // Passed on by b: a's address keeps its time. Sent by a itself: now.
        assert!(net.learn_from(vec![peer_addr(a, 1)], Some(b)).is_empty());
        assert_eq!(net.book_entry(&peer_addr(a, 1)).unwrap().seen, old);
        net.learn_from(vec![peer_addr(a, 1)], Some(a));
        assert!(net.book_entry(&peer_addr(a, 1)).unwrap().seen >= now_secs() - 1);
        // A successful dial vouches too.
        net.vouch(&peer_addr(b, 2));
        assert!(net.book_entry(&peer_addr(b, 2)).unwrap().seen >= now_secs() - 1);

        assert!(net.save_book().unwrap());
        let again = network(8);
        assert_eq!(again.use_book_file(path.clone()).unwrap(), 2);
        assert!(again.book_entry(&peer_addr(a, 1)).unwrap().seen >= now_secs() - 1);

        // Entries age out.
        assert_eq!(again.expire_book_at(now_secs() + BOOK_TTL_SECS + 10), 2);
        assert_eq!(again.book_len(), 0);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_missing_book_file_reads_as_empty() {
        let path = temp_book("missing");
        let net = network(8);
        assert_eq!(net.use_book_file(path.clone()).unwrap(), 0);
        net.learn(vec![addr(1)]);
        assert!(net.save_book().unwrap());
        assert!(std::fs::read_to_string(&path).unwrap().ends_with(" /ip4/10.0.0.1/tcp/8333\n"));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// An `ADDR` carries this node's addresses and a random sample of the
    /// book, not the same entries every time.
    #[test]
    fn shared_addresses_are_a_random_sample() {
        let (node, _rx) = mpsc::channel(1);
        let net = Network::new(node, 8333, vec![addr(0)], 8);
        for start in (1..1_000).step_by(200) {
            net.learn((start..start + 200).map(addr).collect());
        }
        let first = net.known_addrs();
        assert_eq!(first.len(), 1 + ADDR_SAMPLE);
        assert_eq!(first[0], addr(0), "own address first");
        let differ = (0..5).any(|_| {
            let mut x = net.known_addrs();
            let mut y = net.known_addrs();
            x.sort();
            y.sort();
            x != y
        });
        assert!(differ, "the sample changes between messages");
    }

    /// Below the peer cap, the book is dialed at random: never this node or a
    /// connected peer, and an address at most once per `REDIAL_AFTER`.
    #[tokio::test]
    async fn the_book_is_dialed_while_below_the_cap() {
        let own = libp2p::PeerId::random();
        let (node, _rx) = mpsc::channel(1);
        let net = Network::new(node, 8333, vec![peer_addr(own, 0)], 3);
        let (dial_tx, mut dial_rx) = mpsc::channel(100);
        net.set_dialer(dial_tx);
        let connected = libp2p::PeerId::random();
        net.set_connected(connected, true);
        net.connection_opened(
            "direct".to_string(),
            ConnectionInfo {
                peer: connected.to_string(),
                address: String::new(),
                outbound: true,
                relayed: false,
                is_relay: false,
                session: true,
                since: 1,
                label: None,
            },
        );
        let others: Vec<libp2p::PeerId> = (0..10).map(|_| libp2p::PeerId::random()).collect();
        net.learn(vec![peer_addr(own, 99), peer_addr(connected, 98)]);
        net.learn(others.iter().enumerate().map(|(i, p)| peer_addr(*p, i as u32 + 1)).collect());

        net.dial_from_book();
        let mut dialed = Vec::new();
        while let Ok(a) = dial_rx.try_recv() {
            dialed.push(a);
        }
        assert_eq!(dialed.len(), 3, "as many as the cap allows");
        assert!(dialed.iter().all(|a| peer_of(a).is_some_and(|p| others.contains(&p))));

        // The dials fail; the next tick picks others, not the same ones.
        for a in &dialed {
            net.dial_finished(a, false);
        }
        net.dial_from_book();
        let mut next = Vec::new();
        while let Ok(a) = dial_rx.try_recv() {
            next.push(a);
        }
        assert_eq!(next.len(), 3);
        assert!(next.iter().all(|a| !dialed.contains(a)), "tried ones wait REDIAL_AFTER");
    }

    /// Nodes reached through the same relay are separate entries, each named
    /// by its own peer id, not the relay's: each is shared, saved and dialed
    /// on its own, and being connected to the relay does not block them.
    #[tokio::test]
    async fn nodes_behind_one_relay_are_separate_entries() {
        let relay = libp2p::PeerId::random();
        let (a, b) = (libp2p::PeerId::random(), libp2p::PeerId::random());
        let circuit = |p: libp2p::PeerId| -> Multiaddr {
            format!("/ip4/3.14.123.27/tcp/9000/p2p/{relay}/p2p-circuit/p2p/{p}").parse().unwrap()
        };
        let path = temp_book("relayed");
        let net = network(8);
        net.use_book_file(path.clone()).unwrap();
        let (dial_tx, mut dial_rx) = mpsc::channel(16);
        net.set_dialer(dial_tx);
        net.set_connected(relay, true);

        assert_eq!(net.learn(vec![circuit(a), circuit(b)]).len(), 2);
        assert_eq!(net.book_len(), 2);
        assert_eq!(peer_of(&circuit(a)), Some(a), "named by the node, not the relay");
        net.save_book().unwrap();
        let again = network(8);
        assert_eq!(again.use_book_file(path.clone()).unwrap(), 2);

        net.dial_from_book();
        let mut dialed = Vec::new();
        while let Ok(addr) = dial_rx.try_recv() {
            dialed.push(addr);
        }
        dialed.sort();
        let mut both = vec![circuit(a), circuit(b)];
        both.sort();
        assert_eq!(dialed, both);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// A peer connected only through the relay has its direct address dialed
    /// from the book even when that address is not new (it failed before, for
    /// example while the peer's port was closed), with no free slot, and not
    /// again within `REDIAL_AFTER`. A peer reached directly is left alone.
    #[tokio::test]
    async fn a_relayed_peer_is_dialed_directly_again_from_the_book() {
        let public = libp2p::PeerId::random();
        let direct: Multiaddr = format!("/ip4/3.14.123.27/tcp/8333/p2p/{public}").parse().unwrap();
        let net = network(0); // no session slots
        let (dial_tx, mut dial_rx) = mpsc::channel(16);
        net.set_dialer(dial_tx);
        net.learn(vec![direct.clone()]);
        net.set_connected(public, true);
        net.connection_opened(
            "relayed".to_string(),
            ConnectionInfo {
                peer: public.to_string(),
                address: String::new(),
                outbound: false,
                relayed: true,
                is_relay: false,
                session: false,
                since: 1,
                label: None,
            },
        );

        net.dial_from_book();
        assert_eq!(dial_rx.try_recv().ok(), Some(direct.clone()));
        net.dial_finished(&direct, false);
        net.dial_from_book();
        assert!(dial_rx.try_recv().is_err(), "not again within REDIAL_AFTER");

        // Once tried long enough ago it is due again, unless the peer is now
        // reached directly.
        net.inner.book.lock().unwrap().get_mut(&direct).unwrap().tried = None;
        net.connection_opened(
            "direct".to_string(),
            ConnectionInfo {
                peer: public.to_string(),
                address: String::new(),
                outbound: true,
                relayed: false,
                is_relay: false,
                session: false,
                since: 2,
                label: None,
            },
        );
        net.dial_from_book();
        assert!(dial_rx.try_recv().is_err(), "already direct");
    }

    #[test]
    fn peer_of_reads_the_trailing_peer_id() {
        let id = libp2p::PeerId::random();
        let with: Multiaddr = format!("/ip4/127.0.0.1/tcp/1/p2p/{id}").parse().unwrap();
        assert_eq!(peer_of(&with), Some(id));
        assert_eq!(peer_of(&addr(1)), None);
    }
}
