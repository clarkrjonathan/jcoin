//! Per-connection peer logic: the handshake, and (added next) the run loop.

use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::{mpsc, oneshot};
use tokio::time::{interval, Duration, Instant, MissedTickBehavior};

use crate::node::{BlockOutcome, NodeRequest, SubmitResult, BLOCK_BATCH};

use super::frame::PROTOCOL_VERSION;
use super::link::PeerLink;
use super::message::{read_message, write_message, Hello, Message, MessageError};
use super::network::Network;

/// Idle time on a link before sending a `PING`.
pub const PING_INTERVAL: Duration = Duration::from_secs(30);

/// Silence after which a peer is dropped.
pub const PEER_TIMEOUT: Duration = Duration::from_secs(90);

/// Bound on each per-peer channel.
const CHANNEL_BOUND: usize = 64;

/// Lowest wire version this node will talk to.
pub const MIN_PROTOCOL_VERSION: u16 = 1;

/// Errors from the handshake.
#[derive(Debug)]
pub enum HandshakeError {
    Message(MessageError),
    IncompatibleVersion(u16),
    SelfConnection,
    Unexpected(&'static str),
}

impl std::fmt::Display for HandshakeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HandshakeError::Message(e) => write!(f, "{e}"),
            HandshakeError::IncompatibleVersion(v) => {
                write!(f, "peer wire version {v} is not supported")
            }
            HandshakeError::SelfConnection => write!(f, "connected to self (nonce match)"),
            HandshakeError::Unexpected(what) => write!(f, "expected {what} during handshake"),
        }
    }
}

impl std::error::Error for HandshakeError {}

impl From<MessageError> for HandshakeError {
    fn from(e: MessageError) -> Self {
        HandshakeError::Message(e)
    }
}

/// Build this node's `HELLO` for a new connection. The `nonce` must be random
/// per connection; it is what detects a self-connection.
pub fn local_hello(nonce: u64, listen_port: u16) -> Hello {
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    Hello {
        version: PROTOCOL_VERSION,
        nonce,
        timestamp,
        listen_port,
        services: 0,
    }
}

/// Run the symmetric handshake over an established link. Both sides call this.
///
/// Sequence: send `HELLO`, read the peer's `HELLO` and validate it, send
/// `HELLO_ACK`, read the peer's `HELLO_ACK`. On success, return the peer's
/// `HELLO`. The steps are strictly ordered, so no concurrency is needed here;
/// the link is split for the run loop only after this returns.
pub async fn handshake<L>(link: &mut L, local: &Hello) -> Result<Hello, HandshakeError>
where
    L: AsyncRead + AsyncWrite + Unpin,
{
    write_message(link, &Message::Hello(local.clone()))
        .await
        .map_err(MessageError::from)?;

    let peer = match read_message(link).await? {
        Message::Hello(h) => h,
        _ => return Err(HandshakeError::Unexpected("HELLO")),
    };

    if peer.version < MIN_PROTOCOL_VERSION {
        return Err(HandshakeError::IncompatibleVersion(peer.version));
    }
    if peer.nonce == local.nonce {
        return Err(HandshakeError::SelfConnection);
    }

    write_message(link, &Message::HelloAck)
        .await
        .map_err(MessageError::from)?;

    match read_message(link).await? {
        Message::HelloAck => {}
        _ => return Err(HandshakeError::Unexpected("HELLO_ACK")),
    }

    Ok(peer)
}

/// Drive one peer connection to completion: handshake, then serve messages until
/// the link closes or the peer times out.
///
/// `network` is the shared P2P handle: it carries the node-core channel, this
/// node's listen port and advertised addresses, the peer registry, and the
/// address book. `label` is for logs.
pub async fn run_peer<L: PeerLink + 'static>(mut link: L, network: Network, label: String) {
    let nonce = rand::random::<u64>();
    let local = local_hello(nonce, network.listen_port());

    let peer = match handshake(&mut link, &local).await {
        Ok(p) => p,
        Err(e) => {
            eprintln!("[p2p] handshake with {label} failed: {e}");
            return;
        }
    };
    println!(
        "[p2p] connected to {label} (peer wire v{}, port {})",
        peer.version, peer.listen_port
    );

    let (read_half, mut write_half) = tokio::io::split(link);

    // Advertise our own addresses and ask for the peer's, to grow the mesh.
    let _ = write_message(&mut write_half, &Message::Addr(network.known_addrs())).await;
    let _ = write_message(&mut write_half, &Message::GetAddr).await;

    // Ask the peer for any blocks we are missing (initial block download).
    let _ = start_sync(&network, &mut write_half).await;

    // A dedicated reader task keeps frame reads off the select loop, so an
    // in-progress read is never cancelled mid-frame (read_exact is not
    // cancellation-safe). It forwards decoded messages to the loop below.
    let (in_tx, mut in_rx) = mpsc::channel::<Message>(CHANNEL_BOUND);
    let reader = tokio::spawn(read_loop(read_half, in_tx));

    // Outbound channel the network pushes gossip into.
    let (out_tx, mut out_rx) = mpsc::channel::<Message>(CHANNEL_BOUND);
    let id = network.register(out_tx);

    let mut ticker = interval(PING_INTERVAL);
    ticker.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut last_activity = Instant::now();

    loop {
        tokio::select! {
            incoming = in_rx.recv() => {
                match incoming {
                    Some(msg) => {
                        last_activity = Instant::now();
                        if !handle_incoming(msg, id, &network, &mut write_half).await {
                            break;
                        }
                    }
                    None => break, // reader ended: EOF or stream error
                }
            }
            outgoing = out_rx.recv() => {
                if let Some(msg) = outgoing {
                    if write_message(&mut write_half, &msg).await.is_err() {
                        break;
                    }
                }
            }
            _ = ticker.tick() => {
                let idle = last_activity.elapsed();
                if idle >= PEER_TIMEOUT {
                    println!("[p2p] {label} timed out");
                    break;
                }
                if idle >= PING_INTERVAL
                    && write_message(&mut write_half, &Message::Ping(rand::random())).await.is_err()
                {
                    break;
                }
            }
        }
    }

    network.remove(id);
    reader.abort();
    println!("[p2p] disconnected from {label}");
}

/// Read frames until the stream fails. Decode errors that leave the stream in
/// sync (unknown type, bad length, bad transaction) are skipped; a frame-level
/// error (EOF, bad magic, bad checksum) ends the loop.
async fn read_loop<R: AsyncRead + Unpin>(mut read: R, out: mpsc::Sender<Message>) {
    loop {
        match read_message(&mut read).await {
            Ok(msg) => {
                if out.send(msg).await.is_err() {
                    break; // the peer loop is gone
                }
            }
            Err(MessageError::Frame(_)) => break,
            Err(_) => continue,
        }
    }
}

/// Handle one message from a peer. Returns false only on a fatal write error.
async fn handle_incoming<W: AsyncWrite + Unpin>(
    msg: Message,
    peer_id: super::network::PeerId,
    network: &Network,
    write_half: &mut W,
) -> bool {
    match msg {
        Message::Ping(nonce) => write_message(write_half, &Message::Pong(nonce)).await.is_ok(),
        Message::Pong(_) => true,
        Message::NewTransaction(tx) => {
            let (reply_tx, reply_rx) = oneshot::channel();
            if network
                .node()
                .send(NodeRequest::SubmitTransaction {
                    tx: tx.clone(),
                    reply: reply_tx,
                })
                .await
                .is_err()
            {
                return true; // node core gone; nothing to relay
            }
            if let Ok(SubmitResult::Accepted { is_new: true, .. }) = reply_rx.await {
                network.broadcast_transaction(&tx, Some(peer_id));
            }
            true
        }
        // Reply to an address request with what we know.
        Message::GetAddr => write_message(write_half, &Message::Addr(network.known_addrs()))
            .await
            .is_ok(),
        // Learn new addresses and try to connect to fresh direct ones.
        Message::Addr(endpoints) => {
            let fresh = network.learn(endpoints);
            network.maybe_dial(fresh);
            true
        }
        // Submit a received block; relay it if newly stored, or sync the gap if
        // its parent is missing.
        Message::NewBlock(block) => {
            let (reply_tx, reply_rx) = oneshot::channel();
            if network
                .node()
                .send(NodeRequest::SubmitBlock {
                    block: Box::new(block.clone()),
                    reply: reply_tx,
                })
                .await
                .is_err()
            {
                return true;
            }
            match reply_rx.await {
                Ok(BlockOutcome::Accepted { is_new: true, .. }) => {
                    network.broadcast_block(&block, Some(peer_id));
                    true
                }
                Ok(BlockOutcome::Orphan) => start_sync(network, write_half).await,
                _ => true,
            }
        }
        // Answer a sync request with the canonical blocks after the locator.
        Message::GetBlocks(locator) => {
            let (reply_tx, reply_rx) = oneshot::channel();
            if network
                .node()
                .send(NodeRequest::GetBlocksAfter {
                    locator,
                    reply: reply_tx,
                })
                .await
                .is_err()
            {
                return true;
            }
            let Ok(blocks) = reply_rx.await else {
                return true;
            };
            write_message(write_half, &Message::Blocks(blocks)).await.is_ok()
        }
        // Apply a batch of synced blocks in order; if it was full and advanced
        // our chain, ask for the next batch.
        Message::Blocks(blocks) => {
            let count = blocks.len();
            let mut advanced = false;
            for block in blocks {
                let (reply_tx, reply_rx) = oneshot::channel();
                if network
                    .node()
                    .send(NodeRequest::SubmitBlock {
                        block: Box::new(block),
                        reply: reply_tx,
                    })
                    .await
                    .is_err()
                {
                    return true;
                }
                if let Ok(BlockOutcome::Accepted { is_new: true, .. }) = reply_rx.await {
                    advanced = true;
                }
            }
            if advanced && count == BLOCK_BATCH {
                return start_sync(network, write_half).await;
            }
            true
        }
        // A HELLO or HELLO_ACK after the handshake is unexpected; ignore it.
        Message::Hello(_) | Message::HelloAck => true,
    }
}

/// Ask a peer for the blocks we are missing by sending our block locator.
/// Returns false only on a fatal write error.
async fn start_sync<W: AsyncWrite + Unpin>(network: &Network, write_half: &mut W) -> bool {
    let (reply_tx, reply_rx) = oneshot::channel();
    if network
        .node()
        .send(NodeRequest::GetLocator { reply: reply_tx })
        .await
        .is_err()
    {
        return true;
    }
    let Ok(locator) = reply_rx.await else {
        return true;
    };
    write_message(write_half, &Message::GetBlocks(locator))
        .await
        .is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn two_peers_handshake() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        let hello_a = local_hello(1, 8333);
        let hello_b = local_hello(2, 8334);

        let ha = hello_a.clone();
        let hb = hello_b.clone();
        let task_a = tokio::spawn(async move { handshake(&mut a, &ha).await.map(|h| h.nonce) });
        let task_b = tokio::spawn(async move { handshake(&mut b, &hb).await.map(|h| h.nonce) });

        // Each side learns the other's nonce.
        assert_eq!(task_a.await.unwrap().unwrap(), hello_b.nonce);
        assert_eq!(task_b.await.unwrap().unwrap(), hello_a.nonce);
    }

    #[tokio::test]
    async fn self_connection_is_detected() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        let same = local_hello(42, 8333);

        let s1 = same.clone();
        let s2 = same.clone();
        let task_a = tokio::spawn(async move { handshake(&mut a, &s1).await });
        let task_b = tokio::spawn(async move { handshake(&mut b, &s2).await });

        // At least one side must see the nonce clash and reject.
        let ra = task_a.await.unwrap();
        let rb = task_b.await.unwrap();
        assert!(
            matches!(ra, Err(HandshakeError::SelfConnection))
                || matches!(rb, Err(HandshakeError::SelfConnection))
        );
    }
}

/// Two in-process nodes connected over a real TCP socket, exercising handshake,
/// the run loop, and gossip end to end.
#[cfg(test)]
mod gossip_tests {
    use super::run_peer;

    use std::time::Duration;

    use ed25519_dalek::{Signer, SigningKey};
    use rand::rngs::OsRng;
    use tokio::net::{TcpListener, TcpStream};
    use tokio::sync::{mpsc, oneshot};

    use crate::block::{build_and_mine, Block, BLOCK_REWARD, STATIC_TARGET};
    use crate::node::{run_node, BlockOutcome, NodeRequest, StatusResult, SubmitResult};
    use crate::p2p::Network;
    use crate::state::State;
    use crate::Transaction;

    /// A mined block 1 whose coinbase credits `addr` with the block reward.
    fn coinbase_block(addr: [u8; 32]) -> Block {
        let genesis = Block::genesis();
        build_and_mine(&genesis.header, &State::new(), BLOCK_REWARD, addr, None, STATIC_TARGET, 1)
            .unwrap()
            .0
    }

    /// Apply a block directly to one core (not via gossip).
    async fn submit_block(core: &mpsc::Sender<NodeRequest>, block: Block) {
        let (reply, rx) = oneshot::channel();
        core.send(NodeRequest::SubmitBlock {
            block: Box::new(block),
            reply,
        })
        .await
        .unwrap();
        let _ = rx.await;
    }

    /// A signed normal transfer from `key` at `nonce`.
    fn signed_from(key: &SigningKey, nonce: u64) -> Transaction {
        let mut tx = Transaction {
            tx_type: crate::tx_type::NORMAL,
            sender: key.verifying_key().to_bytes(),
            recipient: [5u8; 32],
            amount: 10,
            fee: 0,
            nonce,
            signature: [0u8; 64],
        };
        tx.signature = key.sign(&tx.signing_bytes()).to_bytes();
        tx
    }

    fn spawn_core() -> mpsc::Sender<NodeRequest> {
        let (tx, rx) = mpsc::channel(32);
        tokio::spawn(run_node(rx));
        tx
    }

    async fn is_seen(core: &mpsc::Sender<NodeRequest>, tx_id: &str) -> bool {
        let (reply, rx) = oneshot::channel();
        core.send(NodeRequest::GetStatus {
            tx_id: tx_id.to_string(),
            reply,
        })
        .await
        .unwrap();
        matches!(rx.await.unwrap(), StatusResult::Seen)
    }

    async fn wait_until<F: Fn() -> bool>(check: F) -> bool {
        for _ in 0..100 {
            if check() {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        check()
    }

    async fn chain_height(core: &mpsc::Sender<NodeRequest>) -> u64 {
        let (reply, rx) = oneshot::channel();
        core.send(NodeRequest::GetChainInfo { reply }).await.unwrap();
        rx.await.unwrap().height
    }

    #[tokio::test]
    async fn gossip_propagates_between_two_nodes() {
        let core_a = spawn_core();
        let core_b = spawn_core();
        let net_a = Network::new(core_a.clone(), 1, Vec::new(), 8);
        let net_b = Network::new(core_b.clone(), 2, Vec::new(), 8);

        // A real loopback TCP connection between the two nodes.
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let connect = tokio::spawn(async move { TcpStream::connect(addr).await.unwrap() });
        let (server_stream, _) = listener.accept().await.unwrap();
        let client_stream = connect.await.unwrap();

        tokio::spawn(run_peer(server_stream, net_a.clone(), "A".into()));
        tokio::spawn(run_peer(client_stream, net_b.clone(), "B".into()));

        assert!(
            wait_until(|| net_a.peer_count() == 1 && net_b.peer_count() == 1).await,
            "handshake did not complete"
        );

        // Fund a sender on both nodes with a coinbase block, so the transaction
        // passes nonce and balance validation on each.
        let key = SigningKey::generate(&mut OsRng);
        let block1 = coinbase_block(key.verifying_key().to_bytes());
        submit_block(&core_a, block1.clone()).await;
        submit_block(&core_b, block1).await;

        // Submit to A on the same path the REST layer uses: accept, then relay.
        let tx = signed_from(&key, 0);
        let tx_id = tx.tx_id_hex();
        let (reply, rx) = oneshot::channel();
        core_a
            .send(NodeRequest::SubmitTransaction {
                tx: tx.clone(),
                reply,
            })
            .await
            .unwrap();
        assert!(matches!(
            rx.await.unwrap(),
            SubmitResult::Accepted { is_new: true, .. }
        ));
        net_a.broadcast_transaction(&tx, None);

        // B should receive the gossiped transaction and record it as seen.
        let mut reached_b = false;
        for _ in 0..100 {
            if is_seen(&core_b, &tx_id).await {
                reached_b = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(reached_b, "transaction did not gossip to node B");
    }

    #[tokio::test]
    async fn gossip_through_relay() {
        use crate::p2p::addr::Endpoint;
        use crate::p2p::relay_client::run_relay_client;
        use crate::p2p::relay_server::serve;

        // Start an in-process relay.
        let relay_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let relay_addr = relay_listener.local_addr().unwrap();
        tokio::spawn(serve(relay_listener));

        // Node A connects to the relay (no direct address advertised).
        let core_a = spawn_core();
        let net_a = Network::new(core_a.clone(), 8333, Vec::new(), 8);
        tokio::spawn(run_relay_client(relay_addr, net_a.clone()));

        // Discover A's assigned relay handle from its advertised endpoints.
        let mut handle_a = None;
        for _ in 0..100 {
            if let Some(Endpoint::Relay { handle, .. }) = net_a
                .advertised()
                .into_iter()
                .find(|e| matches!(e, Endpoint::Relay { .. }))
            {
                handle_a = Some(handle);
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let handle_a = handle_a.expect("node A never registered a relay handle");

        // Node B is seeded with A's handle and opens a channel through the relay.
        let core_b = spawn_core();
        let net_b = Network::new(core_b.clone(), 8334, Vec::new(), 8);
        net_b.learn(vec![Endpoint::Relay {
            relay: relay_addr,
            handle: handle_a,
        }]);
        tokio::spawn(run_relay_client(relay_addr, net_b.clone()));

        assert!(
            wait_until(|| net_a.peer_count() >= 1 && net_b.peer_count() >= 1).await,
            "relay channel did not establish"
        );

        // Fund a sender on both nodes.
        let key = SigningKey::generate(&mut OsRng);
        let block1 = coinbase_block(key.verifying_key().to_bytes());
        submit_block(&core_a, block1.clone()).await;
        submit_block(&core_b, block1).await;

        // A transaction submitted to A must reach B through the relay.
        let tx = signed_from(&key, 0);
        let tx_id = tx.tx_id_hex();
        let (reply, rx) = oneshot::channel();
        core_a
            .send(NodeRequest::SubmitTransaction {
                tx: tx.clone(),
                reply,
            })
            .await
            .unwrap();
        assert!(matches!(
            rx.await.unwrap(),
            SubmitResult::Accepted { is_new: true, .. }
        ));
        net_a.broadcast_transaction(&tx, None);

        let mut reached_b = false;
        for _ in 0..150 {
            if is_seen(&core_b, &tx_id).await {
                reached_b = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(reached_b, "transaction did not gossip through the relay to B");
    }

    #[tokio::test]
    async fn mined_block_propagates_to_a_connected_peer() {
        // Both nodes start at genesis and are connected, so a block that extends
        // the tip applies on the peer. (A follower that joins mid-chain and
        // misses a block cannot catch up without chain sync, which is the next
        // milestone.)
        let core_a = spawn_core();
        let core_b = spawn_core();
        let net_a = Network::new(core_a.clone(), 1, Vec::new(), 8);
        let net_b = Network::new(core_b.clone(), 2, Vec::new(), 8);

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let connect = tokio::spawn(async move { TcpStream::connect(addr).await.unwrap() });
        let (server_stream, _) = listener.accept().await.unwrap();
        let client_stream = connect.await.unwrap();
        tokio::spawn(run_peer(server_stream, net_a.clone(), "A".into()));
        tokio::spawn(run_peer(client_stream, net_b.clone(), "B".into()));
        assert!(wait_until(|| net_a.peer_count() == 1 && net_b.peer_count() == 1).await);

        // Mine block 1 and submit it to A; A gossips NEW_BLOCK to B.
        let (block, _) = build_and_mine(
            &Block::genesis().header,
            &State::new(),
            BLOCK_REWARD,
            [7u8; 32],
            None,
            STATIC_TARGET,
            1,
        )
        .unwrap();
        let (reply, rx) = oneshot::channel();
        core_a
            .send(NodeRequest::SubmitBlock {
                block: Box::new(block.clone()),
                reply,
            })
            .await
            .unwrap();
        assert!(matches!(
            rx.await.unwrap(),
            BlockOutcome::Accepted { is_new: true, .. }
        ));
        net_a.broadcast_block(&block, None);

        let mut reached = false;
        for _ in 0..150 {
            if chain_height(&core_b).await == 1 {
                reached = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(reached, "mined block did not propagate to node B");
    }

    #[tokio::test]
    async fn late_joiner_syncs_the_chain() {
        // Node A builds a 5-block chain alone.
        let core_a = spawn_core();
        let net_a = Network::new(core_a.clone(), 1, Vec::new(), 8);
        let mut header = Block::genesis().header;
        let mut state = State::new();
        for _ in 0..5 {
            let (block, new_state) =
                build_and_mine(&header, &state, BLOCK_REWARD, [1u8; 32], None, STATIC_TARGET, 1)
                    .unwrap();
            submit_block(&core_a, block.clone()).await;
            header = block.header;
            state = new_state;
        }
        assert_eq!(chain_height(&core_a).await, 5);

        // Node B joins fresh and should catch up via initial block download.
        let core_b = spawn_core();
        let net_b = Network::new(core_b.clone(), 2, Vec::new(), 8);

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let connect = tokio::spawn(async move { TcpStream::connect(addr).await.unwrap() });
        let (server_stream, _) = listener.accept().await.unwrap();
        let client_stream = connect.await.unwrap();
        tokio::spawn(run_peer(server_stream, net_a.clone(), "A".into()));
        tokio::spawn(run_peer(client_stream, net_b.clone(), "B".into()));

        let mut synced = false;
        for _ in 0..200 {
            if chain_height(&core_b).await == 5 {
                synced = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(synced, "late joiner did not sync to height 5");
    }
}
