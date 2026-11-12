//! The libp2p host: transport, identity, relaying, hole punching, discovery,
//! and the JeffyCoin protocol stream.
//!
//! libp2p replaces the layers below the JeffyCoin protocol, not the protocol
//! itself. Each connection is TCP (or a circuit through a relay) secured with
//! Noise and multiplexed with Yamux. On it, the node opens one stream with the
//! protocol id `PROTOCOL` and runs the unchanged `run_peer` over it: `HELLO`,
//! gossip, sync, and `ADDR` work exactly as over raw TCP, because a libp2p
//! stream is a `PeerLink` like any other.
//!
//! What libp2p adds:
//! - identity: each node has an Ed25519 key; its peer id is the public key,
//!   so a peer cannot be impersonated and a dial to `/p2p/<id>` reaches only
//!   that peer.
//! - encryption: every connection, relayed ones included, is encrypted end to
//!   end between the two nodes. A relay sees only ciphertext.
//! - relaying (circuit relay v2) for nodes behind NAT, and hole punching
//!   (DCUtR) to upgrade a relayed connection to a direct one when possible.
//! - discovery: nodes on a relay register under `NAMESPACE` with the relay's
//!   rendezvous service and discover each other there.
//!
//! The host owns the libp2p `Swarm` in a single task. Other tasks reach it
//! only through channels: `Network::maybe_dial` sends dial requests, and a
//! closed protocol session reports back so the host can reopen it.

use std::collections::HashMap;
use std::error::Error;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::StreamExt;
use libp2p::multiaddr::Protocol;
use libp2p::swarm::dial_opts::{DialOpts, PeerCondition};
use libp2p::swarm::{ConnectionId, NetworkBehaviour, SwarmEvent};
use libp2p::{
    dcutr, identify, identity, noise, ping, relay, rendezvous, tcp, yamux, Multiaddr, PeerId,
    StreamProtocol,
};
use tokio::sync::mpsc;
use tokio_util::compat::FuturesAsyncReadCompatExt;

use super::network::{peer_of, Network};
use super::peer::run_peer;

/// The JeffyCoin protocol id. Version 2 is the first one carried over libp2p
/// (its `ADDR` records are multiaddrs).
pub const PROTOCOL: StreamProtocol = StreamProtocol::new("/jeffycoin/2.0.0");

/// The protocol version string sent in identify.
pub const IDENTIFY_VERSION: &str = "/jeffycoin/2.0.0";

/// The rendezvous namespace JeffyCoin nodes register under.
pub const NAMESPACE: &str = "jeffycoin";

/// How long a connection with no open streams stays up.
const IDLE_TIMEOUT: Duration = Duration::from_secs(300);

/// How often a rendezvous registration is renewed (its TTL is longer).
const REREGISTER_EVERY: Duration = Duration::from_secs(30 * 60);

/// How the host is set up.
pub struct HostConfig {
    /// This node's identity. Its peer id is derived from it.
    pub keypair: identity::Keypair,
    /// Addresses to listen on, for example `/ip4/0.0.0.0/tcp/8333`.
    pub listen: Vec<Multiaddr>,
    /// Extra addresses of this node to advertise (for example a public
    /// address behind port forwarding).
    pub advertise: Vec<Multiaddr>,
    /// Peers to dial on startup and redial whenever they are not connected.
    pub bootstrap: Vec<Multiaddr>,
    /// A relay to reserve a slot on and use for rendezvous discovery. Must
    /// end in `/p2p/<relay peer id>`.
    pub relay: Option<Multiaddr>,
    /// Period of the maintenance tick: bootstrap redial and rendezvous
    /// discovery. Production uses tens of seconds; tests use less.
    pub tick: Duration,
}

/// Load the node identity from `path`, or create one and save it there. The
/// file holds the keypair in libp2p's protobuf encoding. Keep it private: it
/// is the node's identity.
pub fn load_or_create_keypair(path: &Path) -> std::io::Result<identity::Keypair> {
    if let Ok(bytes) = std::fs::read(path) {
        return identity::Keypair::from_protobuf_encoding(&bytes)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e));
    }
    let keypair = identity::Keypair::generate_ed25519();
    let bytes = keypair
        .to_protobuf_encoding()
        .map_err(std::io::Error::other)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(keypair)
}

/// Parse a peer address given as a multiaddr or as `ip:port` (TCP).
pub fn parse_addr(input: &str) -> Result<Multiaddr, String> {
    if let Ok(sock) = input.parse::<std::net::SocketAddr>() {
        return Ok(Multiaddr::empty()
            .with(match sock.ip() {
                std::net::IpAddr::V4(ip) => Protocol::Ip4(ip),
                std::net::IpAddr::V6(ip) => Protocol::Ip6(ip),
            })
            .with(Protocol::Tcp(sock.port())));
    }
    input
        .parse::<Multiaddr>()
        .map_err(|e| format!("'{input}' is neither ip:port nor a multiaddr: {e}"))
}

#[derive(NetworkBehaviour)]
struct Behaviour {
    relay_client: relay::client::Behaviour,
    identify: identify::Behaviour,
    ping: ping::Behaviour,
    dcutr: dcutr::Behaviour,
    rendezvous: rendezvous::client::Behaviour,
    stream: libp2p_stream::Behaviour,
}

/// Count of open (or opening) protocol sessions per peer, shared with the
/// session tasks so the host avoids opening a second one.
type Sessions = Arc<Mutex<HashMap<PeerId, usize>>>;

/// Build the swarm, start listening, and run the host until the process ends.
pub async fn run_host(config: HostConfig, network: Network) -> Result<(), Box<dyn Error + Send + Sync>> {
    let local_id = config.keypair.public().to_peer_id();
    let mut swarm = libp2p::SwarmBuilder::with_existing_identity(config.keypair.clone())
        .with_tokio()
        .with_tcp(
            tcp::Config::default().nodelay(true),
            noise::Config::new,
            yamux::Config::default,
        )?
        .with_relay_client(noise::Config::new, yamux::Config::default)?
        .with_behaviour(|key, relay_client| Behaviour {
            relay_client,
            identify: identify::Behaviour::new(identify::Config::new(
                IDENTIFY_VERSION.to_string(),
                key.public(),
            )),
            ping: ping::Behaviour::default(),
            dcutr: dcutr::Behaviour::new(key.public().to_peer_id()),
            rendezvous: rendezvous::client::Behaviour::new(key.clone()),
            stream: libp2p_stream::Behaviour::new(),
        })?
        .with_swarm_config(|c| c.with_idle_connection_timeout(IDLE_TIMEOUT))
        .build();

    crate::log_info!("[p2p] peer id {local_id}");
    for addr in &config.listen {
        swarm.listen_on(addr.clone())?;
    }
    for addr in &config.advertise {
        let addr = with_peer(addr.clone(), local_id);
        swarm.add_external_address(addr.clone());
        network.add_advertised(addr);
    }

    // Accept inbound protocol streams for the life of the host.
    let mut control = swarm.behaviour().stream.new_control();
    let sessions: Sessions = Arc::default();
    let (closed_tx, mut closed_rx) = mpsc::channel::<PeerId>(64);
    let mut incoming = control
        .accept(PROTOCOL)
        .map_err(|_| "protocol registered twice")?;
    {
        let network = network.clone();
        let sessions = sessions.clone();
        let closed_tx = closed_tx.clone();
        tokio::spawn(async move {
            while let Some((peer, stream)) = incoming.next().await {
                *sessions.lock().unwrap().entry(peer).or_default() += 1;
                tokio::spawn(run_session(
                    stream,
                    peer,
                    "in",
                    network.clone(),
                    sessions.clone(),
                    closed_tx.clone(),
                ));
            }
        });
    }

    // Dial requests from the address book.
    let (dial_tx, mut dial_rx) = mpsc::channel::<Multiaddr>(256);
    let redial_tx = dial_tx.clone();
    network.set_dialer(dial_tx);
    let mut pending: HashMap<ConnectionId, Multiaddr> = HashMap::new();
    // Open connections that run through a relay circuit, by peer. Closed when
    // a direct connection to the same peer opens.
    let mut relayed_conns: HashMap<ConnectionId, PeerId> = HashMap::new();
    // Peers reached only through the relay with a direct dial in flight, and
    // their other direct addresses, tried one at a time if it fails. One dial
    // per address at once would leave a connection per interface (Docker and
    // VM bridges included), and those stay open.
    let mut upgrades: HashMap<PeerId, Vec<Multiaddr>> = HashMap::new();

    // The relay: reserve a slot (listen on a circuit) and use it for discovery.
    // The host first connects to the relay itself, then asks for the
    // reservation on that connection (`reserve`), and asks again whenever the
    // reservation's listener closes. Letting the relay client open its own
    // connection instead races with the host's dial; on Windows the loser
    // fails with "address in use", and if that is the reservation's dial the
    // node stays unreachable for the rest of the run.
    let relay_peer = match &config.relay {
        Some(addr) => {
            let peer = peer_of(addr).ok_or("--relay must end in /p2p/<relay peer id>")?;
            Some((peer, addr.clone()))
        }
        None => None,
    };
    let mut circuit_listener: Option<libp2p::core::transport::ListenerId> = None;
    let namespace = rendezvous::Namespace::from_static(NAMESPACE);
    let mut circuit_addr: Option<Multiaddr> = None;
    let mut registered_at: Option<Instant> = None;
    let mut cookie: Option<rendezvous::Cookie> = None;

    // Bootstrap peers: the peer id learned for each address, once connected.
    let mut bootstrap: HashMap<Multiaddr, Option<PeerId>> = config
        .bootstrap
        .iter()
        .map(|a| (a.clone(), peer_of(a)))
        .collect();

    let mut ticker = tokio::time::interval(config.tick);
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            event = swarm.select_next_some() => match event {
                SwarmEvent::NewListenAddr { address, .. } => {
                    if address.iter().any(|p| p == Protocol::P2pCircuit) {
                        continue; // the relayed address is added on reservation
                    }
                    let address = with_peer(address, local_id);
                    crate::log_info!("[p2p] listening on {address}");
                    network.add_advertised(address);
                }
                SwarmEvent::ExpiredListenAddr { address, .. } => {
                    network.remove_advertised(&with_peer(address, local_id));
                }
                SwarmEvent::ConnectionEstablished { peer_id, connection_id, endpoint, .. } => {
                    network.set_connected(peer_id, true);
                    if let Some(addr) = pending.remove(&connection_id) {
                        network.dial_finished(&addr, true);
                    }
                    if endpoint.is_dialer() {
                        let remote = endpoint.get_remote_address().clone();
                        for (addr, known) in bootstrap.iter_mut() {
                            if known.is_none() && strip_peer(addr) == strip_peer(&remote) {
                                *known = Some(peer_id);
                            }
                        }
                    }
                    let is_relay = relay_peer.as_ref().map(|(p, _)| *p) == Some(peer_id);
                    if let (true, None, Some((_, relay_addr))) = (is_relay, circuit_listener, &relay_peer) {
                        circuit_listener = reserve(&mut swarm, relay_addr);
                    }
                    let remote = endpoint.get_remote_address();
                    // An inbound relayed connection's remote address is only
                    // `/p2p/<peer>`; the circuit shows in the local address.
                    let relayed = endpoint.is_relayed();
                    network.connection_opened(
                        connection_id.to_string(),
                        super::network::ConnectionInfo {
                            peer: peer_id.to_string(),
                            address: remote.to_string(),
                            outbound: endpoint.is_dialer(),
                            relayed,
                            is_relay,
                            session: false,
                            since: std::time::SystemTime::now()
                                .duration_since(std::time::UNIX_EPOCH)
                                .map(|d| d.as_secs())
                                .unwrap_or(0),
                            label: None,
                        },
                    );
                    // A direct connection replaces relayed ones to the same
                    // peer: closing them ends the session on the relay, and
                    // `closed_rx` reopens it on the direct connection.
                    if relayed {
                        relayed_conns.insert(connection_id, peer_id);
                    } else if !is_relay {
                        // Direct addresses still waiting for this peer are not needed.
                        for addr in upgrades.remove(&peer_id).unwrap_or_default() {
                            network.dial_finished(&addr, true);
                        }
                        let old: Vec<ConnectionId> = relayed_conns
                            .iter()
                            .filter(|(_, p)| **p == peer_id)
                            .map(|(c, _)| *c)
                            .collect();
                        if !old.is_empty() {
                            crate::log_info!(
                                [("peer", peer6(&peer_id.to_string())), ("via", "direct".to_string())];
                                "[p2p] direct connection to {peer_id} at {remote}; closing {} relayed connection(s)",
                                old.len()
                            );
                            for c in old {
                                swarm.close_connection(c);
                            }
                        }
                    }
                    // The dialing side opens the protocol stream, unless a
                    // session already runs (for example, a hole-punched direct
                    // connection after a relayed one).
                    if endpoint.is_dialer() && !is_relay {
                        open_session(&control, peer_id, &network, &sessions, &closed_tx);
                    }
                }
                SwarmEvent::OutgoingConnectionError { connection_id, peer_id, error, .. } => {
                    let addr = pending.remove(&connection_id);
                    if let Some(addr) = &addr {
                        network.dial_finished(addr, false);
                        if let Some(peer) = peer_of(addr) {
                            next_upgrade(&mut upgrades, peer, &redial_tx, &network);
                        }
                    }
                    let who = peer_id.map(|p| p.to_string()).unwrap_or_else(|| "?".into());
                    let via = addr.as_ref().map(via).unwrap_or("direct");
                    crate::log_error!(
                        [
                            ("peer", peer6(&who)),
                            ("reason", fail_reason(&error.to_string()).to_string()),
                            ("via", via.to_string()),
                        ];
                        "[p2p] dial to {who} failed: {error}"
                    );
                }
                SwarmEvent::ConnectionClosed { peer_id, connection_id, num_established, .. } => {
                    relayed_conns.remove(&connection_id);
                    network.connection_closed(&connection_id.to_string());
                    if num_established == 0 {
                        network.set_connected(peer_id, false);
                    }
                }
                SwarmEvent::ListenerClosed { listener_id, reason, .. } if Some(listener_id) == circuit_listener => {
                    // The reservation is gone (the relay refused it, or the
                    // connection to the relay dropped). The next tick asks again.
                    let why = match reason {
                        Ok(()) => "closed".to_string(),
                        Err(e) => e.to_string(),
                    };
                    crate::log_error!(
                        [("reason", fail_reason(&why).to_string())];
                        "[p2p] relay reservation lost ({why}); this node cannot be reached until it is renewed"
                    );
                    circuit_listener = None;
                    if let Some(addr) = circuit_addr.take() {
                        swarm.remove_external_address(&addr);
                        network.remove_advertised(&addr);
                    }
                    registered_at = None;
                }
                SwarmEvent::ListenerError { listener_id, error } if Some(listener_id) == circuit_listener => {
                    crate::log_error!("[p2p] relay reservation error: {error}");
                }
                SwarmEvent::Behaviour(BehaviourEvent::RelayClient(
                    relay::client::Event::ReservationReqAccepted { relay_peer_id, renewal, .. },
                )) => {
                    if let Some((_, relay_addr)) = &relay_peer {
                        let addr = relay_addr
                            .clone()
                            .with(Protocol::P2pCircuit)
                            .with(Protocol::P2p(local_id));
                        if !renewal {
                            crate::log_info!("[p2p] relay reservation accepted by {relay_peer_id}; reachable at {addr}");
                        }
                        swarm.add_external_address(addr.clone());
                        network.add_advertised(addr.clone());
                        circuit_addr = Some(addr);
                        registered_at = None; // register (again) on the next tick
                    }
                }
                SwarmEvent::Behaviour(BehaviourEvent::Dcutr(dcutr::Event { remote_peer_id, result })) => {
                    match result {
                        Ok(_) => crate::log_info!(
                            [("peer", peer6(&remote_peer_id.to_string())), ("via", "direct".to_string())];
                            "[p2p] hole punch to {remote_peer_id} succeeded: direct connection"
                        ),
                        Err(e) => crate::log_info!(
                            [("peer", peer6(&remote_peer_id.to_string())), ("via", "relay".to_string())];
                            "[p2p] hole punch to {remote_peer_id} failed ({e}); staying on the relay"
                        ),
                    }
                }
                SwarmEvent::Behaviour(BehaviourEvent::Rendezvous(event)) => match event {
                    rendezvous::client::Event::Registered { ttl, .. } => {
                        crate::log_info!("[p2p] registered with the relay's rendezvous for {ttl} s");
                    }
                    rendezvous::client::Event::RegisterFailed { error, .. } => {
                        crate::log_error!("[p2p] rendezvous registration failed: {error:?}");
                        registered_at = None;
                    }
                    rendezvous::client::Event::Discovered { registrations, cookie: next, .. } => {
                        cookie = Some(next);
                        let mut addrs = Vec::new();
                        for reg in registrations {
                            let peer = reg.record.peer_id();
                            if peer == local_id || network.is_connected(&peer) {
                                continue;
                            }
                            addrs.extend(reg.record.addresses().iter().map(|a| with_peer(a.clone(), peer)));
                        }
                        let fresh = network.learn(addrs.clone());
                        network.maybe_dial(fresh);
                    }
                    rendezvous::client::Event::DiscoverFailed { error, .. } => {
                        crate::log_error!("[p2p] rendezvous discovery failed: {error:?}");
                        cookie = None;
                    }
                    rendezvous::client::Event::Expired { .. } => {}
                },
                _ => {}
            },

            Some(addr) = dial_rx.recv() => {
                // A direct address of a peer reached only through the relay
                // (`Network::maybe_dial` let it through): dial even though
                // the peer is connected.
                let upgrade_peer = peer_of(&addr).filter(|p| via(&addr) == "direct" && swarm.is_connected(p));
                if let Some(peer) = upgrade_peer {
                    if network.has_direct_connection(&peer) {
                        network.dial_finished(&addr, true);
                        continue;
                    }
                    if let Some(waiting) = upgrades.get_mut(&peer) {
                        waiting.push(addr); // one direct dial per peer at a time
                        continue;
                    }
                    upgrades.insert(peer, Vec::new());
                }
                let opts = dial_opts(&addr, upgrade_peer.is_some());
                let id = opts.connection_id();
                match swarm.dial(opts) {
                    Ok(()) => { pending.insert(id, addr); }
                    // Already connected or a dial is in flight: not a failure,
                    // and logging it was most of the noise. Stay quiet.
                    Err(libp2p::swarm::DialError::DialPeerConditionFalse(_)) => {
                        network.dial_finished(&addr, false);
                    }
                    Err(e) => {
                        network.dial_finished(&addr, false);
                        if let Some(peer) = upgrade_peer {
                            next_upgrade(&mut upgrades, peer, &redial_tx, &network);
                        }
                        crate::log_error!(
                            [
                                ("peer", peer_of(&addr).map(|p| peer6(&p.to_string())).unwrap_or_else(|| "?".into())),
                                ("reason", fail_reason(&e.to_string()).to_string()),
                                ("via", via(&addr).to_string()),
                            ];
                            "[p2p] cannot dial {addr}: {e}"
                        );
                    }
                }
            }

            Some(peer) = closed_rx.recv() => {
                // A session ended but the peer is still connected (for example,
                // its relayed connection closed after a hole punch). Reopen on
                // the remaining connection; the smaller peer id opens, so the
                // two sides do not both reopen.
                let open = sessions.lock().unwrap().get(&peer).copied().unwrap_or(0);
                if open == 0 && swarm.is_connected(&peer) && local_id < peer {
                    open_session(&control, peer, &network, &sessions, &closed_tx);
                }
            }

            _ = ticker.tick() => {
                for (addr, known) in &bootstrap {
                    let connected = known.map(|p| swarm.is_connected(&p)).unwrap_or(false);
                    if !connected {
                        let _ = swarm.dial(dial_opts(addr, false));
                    }
                }
                if let Some((relay_id, relay_addr)) = &relay_peer {
                    if !swarm.is_connected(relay_id) {
                        let _ = swarm.dial(dial_opts(relay_addr, false));
                        continue;
                    }
                    if circuit_listener.is_none() {
                        circuit_listener = reserve(&mut swarm, relay_addr);
                    }
                    let due = registered_at.is_none_or(|t| t.elapsed() >= REREGISTER_EVERY);
                    if circuit_addr.is_some() && due {
                        match swarm.behaviour_mut().rendezvous.register(namespace.clone(), *relay_id, None) {
                            Ok(()) => registered_at = Some(Instant::now()),
                            Err(e) => crate::log_error!("[p2p] cannot register with rendezvous: {e}"),
                        }
                    }
                    swarm.behaviour_mut().rendezvous.discover(
                        Some(namespace.clone()),
                        cookie.clone(),
                        None,
                        *relay_id,
                    );
                }
            }
        }
    }
}

/// Open the protocol stream to `peer` and run a session on it, unless one is
/// already open or opening.
fn open_session(
    control: &libp2p_stream::Control,
    peer: PeerId,
    network: &Network,
    sessions: &Sessions,
    closed_tx: &mpsc::Sender<PeerId>,
) {
    {
        let mut map = sessions.lock().unwrap();
        let count = map.entry(peer).or_default();
        if *count > 0 {
            return;
        }
        *count += 1; // reserve before the async open, so it is not opened twice
    }
    let mut control = control.clone();
    let network = network.clone();
    let sessions = sessions.clone();
    let closed_tx = closed_tx.clone();
    tokio::spawn(async move {
        match control.open_stream(peer, PROTOCOL).await {
            Ok(stream) => run_session(stream, peer, "out", network, sessions, closed_tx).await,
            Err(e) => {
                release(&sessions, peer);
                crate::log_error!("[p2p] could not open a protocol stream to {peer}: {e}");
                // The connection it was opening on may have closed (for
                // example, a relayed one replaced by a direct one). Let the
                // host reopen on what remains, after a pause so a peer that
                // keeps refusing is not retried in a tight loop.
                tokio::time::sleep(Duration::from_secs(1)).await;
                let _ = closed_tx.send(peer).await;
            }
        }
    });
}

/// Run `run_peer` on a protocol stream. The caller has already counted the
/// session; it is released when the session ends.
async fn run_session(
    stream: libp2p::Stream,
    peer: PeerId,
    direction: &'static str,
    network: Network,
    sessions: Sessions,
    closed_tx: mpsc::Sender<PeerId>,
) {
    run_peer(stream.compat(), network, format!("{peer} ({direction})")).await;
    release(&sessions, peer);
    let _ = closed_tx.send(peer).await;
}

fn release(sessions: &Sessions, peer: PeerId) {
    let mut map = sessions.lock().unwrap();
    if let Some(count) = map.get_mut(&peer) {
        *count = count.saturating_sub(1);
        if *count == 0 {
            map.remove(&peer);
        }
    }
}

/// Ask the relay at `relay_addr` for a reservation by listening on a circuit
/// through it. The host must already be connected to the relay. Returns the
/// listener, or `None` (logged) if the request could not be made.
fn reserve(swarm: &mut libp2p::Swarm<Behaviour>, relay_addr: &Multiaddr) -> Option<libp2p::core::transport::ListenerId> {
    match swarm.listen_on(relay_addr.clone().with(Protocol::P2pCircuit)) {
        Ok(id) => Some(id),
        Err(e) => {
            crate::log_error!("[p2p] cannot request a relay reservation: {e}");
            None
        }
    }
}

/// Options for dialing `addr`.
///
/// By default libp2p dials from the listen port, which hole punching relies
/// on. Windows does not allow a second socket to connect from a port that is
/// in use the way Unix does, and such dials fail with "Only one usage of each
/// socket address" (os error 10048), so on Windows every dial gets a new port.
/// A direct dial to `peer` (reached only through the relay) failed: hand its
/// waiting addresses back to the dial queue. The first one becomes the next
/// dial and the rest wait behind it again.
fn next_upgrade(
    upgrades: &mut HashMap<PeerId, Vec<Multiaddr>>,
    peer: PeerId,
    redial: &mpsc::Sender<Multiaddr>,
    network: &Network,
) {
    for addr in upgrades.remove(&peer).unwrap_or_default() {
        if redial.try_send(addr.clone()).is_err() {
            network.dial_finished(&addr, false);
        }
    }
}

fn dial_opts(addr: &Multiaddr, even_if_connected: bool) -> DialOpts {
    match peer_of(addr) {
        Some(peer) => {
            let mut opts = DialOpts::peer_id(peer);
            if even_if_connected {
                opts = opts.condition(PeerCondition::Always);
            }
            let opts = opts.addresses(vec![addr.clone()]);
            if cfg!(windows) { opts.allocate_new_port().build() } else { opts.build() }
        }
        None => {
            let opts = DialOpts::unknown_peer_id().address(addr.clone());
            if cfg!(windows) { opts.allocate_new_port().build() } else { opts.build() }
        }
    }
}

/// `addr` with `/p2p/<peer>` at the end (unless it already ends in a peer id).
fn with_peer(addr: Multiaddr, peer: PeerId) -> Multiaddr {
    if peer_of(&addr).is_some() {
        addr
    } else {
        addr.with(Protocol::P2p(peer))
    }
}

/// `addr` without a trailing `/p2p/<id>`.
fn strip_peer(addr: &Multiaddr) -> Multiaddr {
    let mut addr = addr.clone();
    if peer_of(&addr).is_some() {
        addr.pop();
    }
    addr
}

/// The 6 characters after the `12D3KooW` prefix of a peer id, for the `peer`
/// log attribute. One run of one peer can then be picked out without the full
/// 52-character id. Falls back to the first 6 characters.
pub(crate) fn peer6(peer: &str) -> String {
    peer.strip_prefix("12D3KooW")
        .unwrap_or(peer)
        .chars()
        .take(6)
        .collect()
}

/// Whether an address reaches the peer through the relay (`/p2p-circuit`) or
/// directly, for the `via` log attribute.
fn via(addr: &Multiaddr) -> &'static str {
    if addr.iter().any(|p| matches!(p, Protocol::P2pCircuit)) {
        "relay"
    } else {
        "direct"
    }
}

/// A short class for a dial, connection, or reservation failure, for the
/// `reason` log attribute. Keeps the full text in the message.
fn fail_reason(error: &str) -> &'static str {
    let e = error.to_ascii_lowercase();
    if e.contains("timed out") || e.contains("timeout") {
        "timeout"
    } else if e.contains("refused") {
        "refused"
    } else if e.contains("reservation") || e.contains("circuit") || e.contains("relay") {
        "no_reservation"
    } else if e.contains("in use") || e.contains("10048") || e.contains("already in use") {
        "address_in_use"
    } else if e.contains("handshake") || e.contains("negotiat") {
        "handshake"
    } else {
        "other"
    }
}

/// Hosts on loopback: a direct connection, and relay-only nodes that find each
/// other through rendezvous and talk over a relay circuit.
#[cfg(test)]
mod tests {
    use super::*;

    use ed25519_dalek::{Signer, SigningKey};
    use rand::rngs::OsRng;
    use tokio::sync::oneshot;

    use crate::block::{build_and_mine, Block};
    use crate::consensus::REGTEST;
    use crate::node::{run_node, NodeRequest, StatusResult, SubmitResult};
    use crate::state::State;
    use crate::Transaction;

    const TICK: Duration = Duration::from_millis(200);

    fn spawn_core() -> mpsc::Sender<NodeRequest> {
        let (tx, rx) = mpsc::channel(32);
        tokio::spawn(run_node(rx, None, REGTEST));
        tx
    }

    /// Start a host and return its network handle and peer id.
    fn spawn_host(listen: Vec<Multiaddr>, bootstrap: Vec<Multiaddr>, relay: Option<Multiaddr>) -> (Network, PeerId, mpsc::Sender<NodeRequest>) {
        let core = spawn_core();
        let network = Network::new(core.clone(), 0, Vec::new(), 8);
        let keypair = identity::Keypair::generate_ed25519();
        let id = keypair.public().to_peer_id();
        let config = HostConfig { keypair, listen, advertise: Vec::new(), bootstrap, relay, tick: TICK };
        let net = network.clone();
        tokio::spawn(async move { run_host(config, net).await.unwrap() });
        (network, id, core)
    }

    async fn wait_for<F: Fn() -> bool>(what: &str, check: F) {
        for _ in 0..300 {
            if check() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("timed out waiting for {what}");
    }

    /// Fund a fresh key on every core with the same block 1, then submit a
    /// transfer to the first core and relay it the way the REST layer does.
    /// Returns the transaction id.
    async fn send_tx(cores: &[&mpsc::Sender<NodeRequest>], from: &Network) -> String {
        let key = SigningKey::generate(&mut OsRng);
        let genesis = Block::genesis(&REGTEST);
        let (block, _) = build_and_mine(
            &genesis.header,
            &State::new(),
            REGTEST.initial_reward,
            key.verifying_key().to_bytes(),
            None,
            REGTEST.pow_limit,
            1,
        )
        .unwrap();
        for core in cores {
            let (reply, rx) = oneshot::channel();
            core.send(NodeRequest::SubmitBlock { block: Box::new(block.clone()), reply })
                .await
                .unwrap();
            let _ = rx.await;
        }
        let mut tx = Transaction {
            tx_type: crate::tx_type::NORMAL,
            sender: key.verifying_key().to_bytes(),
            recipient: [5u8; 32],
            amount: 10,
            fee: 0,
            nonce: 0,
            signature: [0u8; 64],
        };
        tx.signature = key.sign(&tx.signing_bytes()).to_bytes();
        let (reply, rx) = oneshot::channel();
        cores[0]
            .send(NodeRequest::SubmitTransaction { tx: tx.clone(), reply })
            .await
            .unwrap();
        assert!(matches!(rx.await.unwrap(), SubmitResult::Accepted { is_new: true, .. }));
        from.broadcast_transaction(&tx, None);
        tx.tx_id_hex()
    }

    async fn wait_seen(core: &mpsc::Sender<NodeRequest>, tx_id: &str) {
        for _ in 0..200 {
            let (reply, rx) = oneshot::channel();
            core.send(NodeRequest::GetStatus { tx_id: tx_id.to_string(), reply })
                .await
                .unwrap();
            if matches!(rx.await.unwrap(), StatusResult::Pending | StatusResult::Seen) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("transaction {tx_id} never arrived");
    }

    fn loopback() -> Multiaddr {
        "/ip4/127.0.0.1/tcp/0".parse().unwrap()
    }

    #[tokio::test]
    async fn two_hosts_connect_directly_and_gossip() {
        let (net_a, id_a, core_a) = spawn_host(vec![loopback()], Vec::new(), None);
        wait_for("A to listen", || !net_a.advertised().is_empty()).await;
        let addr_a = net_a.advertised()[0].clone();
        assert_eq!(peer_of(&addr_a), Some(id_a), "advertised address names the peer");

        let (net_b, _, core_b) = spawn_host(vec![loopback()], vec![addr_a], None);
        wait_for("a protocol session on both sides", || {
            net_a.peer_count() == 1 && net_b.peer_count() == 1
        })
        .await;

        let tx_id = send_tx(&[&core_a, &core_b], &net_a).await;
        wait_seen(&core_b, &tx_id).await;
    }

    #[tokio::test]
    async fn relay_only_hosts_discover_each_other_and_gossip() {
        // A relay on a free loopback port.
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let relay_listen: Multiaddr = format!("/ip4/127.0.0.1/tcp/{port}").parse().unwrap();
        let relay_key = identity::Keypair::generate_ed25519();
        let relay_id = relay_key.public().to_peer_id();
        let listen = relay_listen.clone();
        tokio::spawn(async move {
            super::super::relay_host::run_relay(relay_key, vec![listen.clone()], vec![listen])
                .await
                .unwrap()
        });
        let relay_addr = relay_listen.with(Protocol::P2p(relay_id));

        // Two nodes with no listen address at all: reachable only through the
        // relay, and knowing nothing but the relay.
        let (net_a, _, core_a) = spawn_host(Vec::new(), Vec::new(), Some(relay_addr.clone()));
        let (net_b, _, core_b) = spawn_host(Vec::new(), Vec::new(), Some(relay_addr));

        wait_for("both reservations", || {
            let relayed = |n: &Network| n.advertised().iter().any(|a| a.iter().any(|p| p == Protocol::P2pCircuit));
            relayed(&net_a) && relayed(&net_b)
        })
        .await;
        wait_for("rendezvous discovery and a relayed session", || {
            net_a.peer_count() >= 1 && net_b.peer_count() >= 1
        })
        .await;

        let tx_id = send_tx(&[&core_a, &core_b], &net_a).await;
        wait_seen(&core_b, &tx_id).await;
    }
}
