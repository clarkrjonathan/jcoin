//! JeffyCoin node binary.
//!
//! Runs three things over one Tokio runtime: the node-core task (owns state),
//! an axum REST server (wallet-facing), and the P2P layer (a TCP listener plus
//! a dialer per configured peer). Transactions that arrive by REST or by gossip
//! are validated by the core and, when new, relayed to the other peers.

use std::net::SocketAddr;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use clap::Parser;
use serde_json::json;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};

use jcoin::block::{assemble_block, mine_chunk, Block};
use jcoin::consensus::{MAINNET, REGTEST};
use jcoin::node::{
    run_node, BlockOutcome, ChainInfo, MiningJob, NodeRequest, StatusResult, SubmitResult,
};
use jcoin::p2p::{run_peer, run_relay_client, Endpoint, Network};

#[derive(Parser)]
#[command(name = "jcoin-node", about = "JeffyCoin node")]
struct Cli {
    /// Address for the wallet-facing REST server.
    #[arg(long, default_value = "0.0.0.0:3000")]
    rest_addr: String,
    /// Address for the P2P TCP listener.
    #[arg(long, default_value = "0.0.0.0:8333")]
    p2p_addr: String,
    /// Peer address to dial on startup (repeatable).
    #[arg(long = "peer")]
    peers: Vec<String>,
    /// A directly reachable address of this node to advertise (repeatable). A
    /// concrete (non-wildcard) --p2p-addr is advertised automatically.
    #[arg(long = "advertise")]
    advertise: Vec<String>,
    /// Soft cap on peers before the auto-dialer stops reaching out.
    #[arg(long, default_value_t = 8)]
    max_peers: usize,
    /// Relay address to connect through for peers behind carrier-grade NAT.
    #[arg(long)]
    relay: Option<String>,
    /// A peer's relay directory handle (hex) to open a channel to on startup,
    /// used to bootstrap before handles are learned via gossip. Repeatable.
    #[arg(long = "relay-open")]
    relay_open: Vec<String>,
    /// Mine blocks. Requires --miner.
    #[arg(long)]
    mine: bool,
    /// Address (hex public key, 32 bytes) that receives coinbase rewards.
    #[arg(long)]
    miner: Option<String>,
    /// Directory to persist the chain in (an append-only block log). Omit to run
    /// in memory only. Each node needs its own directory.
    #[arg(long)]
    data_dir: Option<std::path::PathBuf>,
    /// Run on the local test network (easy proof of work, its own genesis)
    /// instead of the real one. Regtest and real nodes reject each other's
    /// blocks.
    #[arg(long)]
    regtest: bool,
}

/// Shared state for REST handlers: a channel into the node core and the peer set.
#[derive(Clone)]
struct AppState {
    node: mpsc::Sender<NodeRequest>,
    network: Network,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let p2p_sock: SocketAddr = cli
        .p2p_addr
        .parse()
        .expect("p2p-addr must be an ip:port");
    let listen_port = p2p_sock.port();

    // Node core.
    let (node_tx, node_rx) = mpsc::channel::<NodeRequest>(100);
    let params = if cli.regtest { REGTEST } else { MAINNET };
    tokio::spawn(run_node(node_rx, cli.data_dir.clone(), params));

    // Addresses this node advertises: the configured ones, plus a concrete
    // --p2p-addr.
    let mut advertised: Vec<Endpoint> = cli
        .advertise
        .iter()
        .map(|s| Endpoint::Direct(s.parse().expect("advertise must be ip:port")))
        .collect();
    if !p2p_sock.ip().is_unspecified() {
        let own = Endpoint::Direct(p2p_sock);
        if !advertised.contains(&own) {
            advertised.push(own);
        }
    }

    let network = Network::new(node_tx.clone(), listen_port, advertised, cli.max_peers);

    // P2P listener.
    tokio::spawn(p2p_listen(p2p_sock, network.clone()));

    // One dialer per configured peer; each retries on disconnect.
    for peer in cli.peers {
        tokio::spawn(p2p_dial(peer, network.clone()));
    }

    // Optional relay client for peers behind carrier-grade NAT.
    if let Some(relay) = cli.relay {
        let relay_sock: SocketAddr = relay.parse().expect("relay must be an ip:port");
        // Seed bootstrap handles into the book so the client opens them once it
        // registers.
        let seeds: Vec<Endpoint> = cli
            .relay_open
            .iter()
            .map(|h| {
                let bytes = hex::decode(h).expect("relay-open must be hex");
                let handle: jcoin::p2p::Handle =
                    bytes.try_into().expect("relay-open handle must be 16 bytes");
                Endpoint::Relay {
                    relay: relay_sock,
                    handle,
                }
            })
            .collect();
        if !seeds.is_empty() {
            network.learn(seeds);
        }
        tokio::spawn(run_relay_client(relay_sock, network.clone()));
    } else if !cli.relay_open.is_empty() {
        eprintln!("--relay-open requires --relay");
    }

    // Optional miner.
    if cli.mine {
        let miner_hex = cli.miner.expect("--mine requires --miner <hex address>");
        let bytes = hex::decode(&miner_hex).expect("--miner must be hex");
        let miner: [u8; 32] = bytes.try_into().expect("--miner must be 32 bytes");
        tokio::spawn(mine_loop(network.clone(), miner));
    } else if cli.miner.is_some() {
        eprintln!("--miner has no effect without --mine");
    }

    let state = AppState {
        node: node_tx,
        network,
    };

    let app = Router::new()
        .route("/wallet/transaction", post(submit_transaction))
        .route("/wallet/transaction/{tx_id}", get(transaction_status))
        .route("/balance/{address}", get(balance))
        .route("/chain", get(chain_info))
        .with_state(state);

    let listener = TcpListener::bind(&cli.rest_addr)
        .await
        .expect("failed to bind REST address");

    println!(
        "JeffyCoin node: REST on {}, P2P on {}",
        cli.rest_addr, cli.p2p_addr
    );
    axum::serve(listener, app).await.expect("server error");
}

/// Accept inbound P2P connections and run each as a peer.
async fn p2p_listen(addr: SocketAddr, network: Network) {
    let listener = match TcpListener::bind(addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("[p2p] failed to bind {addr}: {e}");
            return;
        }
    };
    println!("[p2p] listening on {addr}");

    loop {
        match listener.accept().await {
            Ok((stream, peer_addr)) => {
                tokio::spawn(run_peer(
                    stream,
                    network.clone(),
                    format!("{peer_addr} (in)"),
                ));
            }
            Err(e) => eprintln!("[p2p] accept error: {e}"),
        }
    }
}

/// Mine blocks continuously, crediting `miner`. Each round asks the core for a
/// job (tip + state + an applicable mempool transaction), mines off the runtime
/// with `spawn_blocking`, submits the result, and gossips it when accepted. A
/// short pace between rounds keeps a fast static target from flooding blocks.
async fn mine_loop(network: Network, miner: [u8; 32]) {
    // Nonces tried per blocking burst before checking whether to keep mining.
    const CHUNK: u64 = 200_000;

    loop {
        // Fetch a fresh job: the current tip, its state, and a mempool tx if any.
        let (job_tx, job_rx) = oneshot::channel();
        if network
            .node()
            .send(NodeRequest::GetMiningJob { reply: job_tx })
            .await
            .is_err()
        {
            return;
        }
        let Ok(MiningJob {
            parent,
            parent_state,
            normal,
            context,
        }) = job_rx.await
        else {
            return;
        };
        let target_height = context.height;

        // Never below the consensus minimum: a clock behind median-time-past
        // would otherwise build blocks the chain rejects, forever.
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let timestamp = now.max(context.min_timestamp);
        let (mut header, transactions, _state) = match assemble_block(
            &parent,
            &parent_state,
            context.reward,
            miner,
            normal,
            context.target,
            timestamp,
        ) {
            Ok(parts) => parts,
            Err(e) => {
                eprintln!("[mine] assemble failed: {e}");
                tokio::time::sleep(Duration::from_millis(500)).await;
                continue;
            }
        };

        // Mine in chunks off the runtime; between chunks, give up if someone
        // else already extended the chain to our target height.
        loop {
            let (returned, found) = tokio::task::spawn_blocking(move || {
                let hit = mine_chunk(&mut header, CHUNK);
                (header, hit)
            })
            .await
            .expect("mining task panicked");
            header = returned;

            if found {
                let block = Block {
                    header: header.clone(),
                    transactions: transactions.clone(),
                };
                println!(
                    "[mine] found candidate height {} ({} txs) {}",
                    block.header.height,
                    block.transactions.len(),
                    hex::encode(block.hash())
                );
                let (ack_tx, ack_rx) = oneshot::channel();
                if network
                    .node()
                    .send(NodeRequest::SubmitBlock {
                        block: Box::new(block.clone()),
                        reply: ack_tx,
                    })
                    .await
                    .is_err()
                {
                    return;
                }
                match ack_rx.await {
                    Ok(BlockOutcome::Accepted { is_new: true, height, .. }) => {
                        println!("[mine] won block {height} {}", hex::encode(block.hash()));
                        network.broadcast_block(&block, None);
                    }
                    Ok(BlockOutcome::Accepted { is_new: false, height, .. }) => {
                        println!("[mine] candidate at height {height} already known");
                    }
                    Ok(BlockOutcome::Rejected { reason }) => {
                        println!("[mine] candidate REJECTED: {reason}");
                    }
                    Ok(BlockOutcome::Orphan) => {
                        println!("[mine] candidate ORPHAN (parent unknown)");
                    }
                    Err(_) => return,
                }
                break;
            }

            // Someone else may have found this height while we hashed.
            let (info_tx, info_rx) = oneshot::channel();
            if network
                .node()
                .send(NodeRequest::GetChainInfo { reply: info_tx })
                .await
                .is_err()
            {
                return;
            }
            if let Ok(ChainInfo { height, .. }) = info_rx.await {
                if height >= target_height {
                    // Lost the race for this height; abandon and get a new job.
                    break;
                }
            }
        }
    }
}

/// Dial a configured peer and run it; on disconnect, wait and retry.
async fn p2p_dial(addr: String, network: Network) {
    loop {
        match TcpStream::connect(&addr).await {
            Ok(stream) => {
                run_peer(stream, network.clone(), format!("{addr} (out)")).await;
            }
            Err(e) => eprintln!("[p2p] dial {addr} failed: {e}"),
        }
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

/// POST /wallet/transaction
///
/// A malformed body is rejected by the extractor with 400 before this runs. A
/// valid but unaccepted transaction returns 200 with `status: rejected`. A newly
/// accepted transaction is gossiped to all peers.
async fn submit_transaction(
    State(state): State<AppState>,
    Json(tx): Json<jcoin::Transaction>,
) -> impl IntoResponse {
    let (reply_tx, reply_rx) = oneshot::channel();

    if state
        .node
        .send(NodeRequest::SubmitTransaction {
            tx: tx.clone(),
            reply: reply_tx,
        })
        .await
        .is_err()
    {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "node core unavailable" })),
        );
    }

    match reply_rx.await {
        Ok(SubmitResult::Accepted { tx_id, is_new }) => {
            if is_new {
                state.network.broadcast_transaction(&tx, None);
            }
            (
                StatusCode::OK,
                Json(json!({ "status": "accepted", "relayed": is_new, "tx_id": tx_id })),
            )
        }
        Ok(SubmitResult::Rejected { reason }) => (
            StatusCode::OK,
            Json(json!({ "status": "rejected", "relayed": false, "reason": reason })),
        ),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "no response from node core" })),
        ),
    }
}

/// GET /wallet/transaction/{tx_id} — `pending` (in the mempool), `seen` (seen
/// recently, no longer pending; usually included in a block), or `unknown`.
async fn transaction_status(
    State(state): State<AppState>,
    Path(tx_id): Path<String>,
) -> impl IntoResponse {
    let (reply_tx, reply_rx) = oneshot::channel();

    if state
        .node
        .send(NodeRequest::GetStatus {
            tx_id: tx_id.clone(),
            reply: reply_tx,
        })
        .await
        .is_err()
    {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "node core unavailable" })),
        );
    }

    match reply_rx.await {
        Ok(StatusResult::Pending) => (
            StatusCode::OK,
            Json(json!({ "tx_id": tx_id, "status": "pending" })),
        ),
        Ok(StatusResult::Seen) => (
            StatusCode::OK,
            Json(json!({ "tx_id": tx_id, "status": "seen" })),
        ),
        Ok(StatusResult::Unknown) => (
            StatusCode::NOT_FOUND,
            Json(json!({ "tx_id": tx_id, "status": "unknown" })),
        ),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "no response from node core" })),
        ),
    }
}

/// GET /balance/{address} — the account's balance and next nonce from the tip.
async fn balance(State(state): State<AppState>, Path(address): Path<String>) -> impl IntoResponse {
    let parsed: Option<[u8; 32]> = hex::decode(&address)
        .ok()
        .and_then(|b| b.try_into().ok());
    let Some(addr) = parsed else {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "address must be 32 hex bytes" })),
        );
    };

    let (reply_tx, reply_rx) = oneshot::channel();
    if state
        .node
        .send(NodeRequest::GetAccount {
            address: addr,
            reply: reply_tx,
        })
        .await
        .is_err()
    {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "node core unavailable" })),
        );
    }

    let (balance, nonce) = reply_rx.await.unwrap_or((0, 0));
    (
        StatusCode::OK,
        Json(json!({ "address": address, "balance": balance, "nonce": nonce })),
    )
}

/// GET /chain — chain height and tip hash.
async fn chain_info(State(state): State<AppState>) -> impl IntoResponse {
    let (reply_tx, reply_rx) = oneshot::channel();
    if state
        .node
        .send(NodeRequest::GetChainInfo { reply: reply_tx })
        .await
        .is_err()
    {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "node core unavailable" })),
        );
    }
    let ChainInfo {
        height,
        tip_hash,
        tip_target,
    } = reply_rx.await.unwrap_or(ChainInfo {
        height: 0,
        tip_hash: [0u8; 32],
        tip_target: [0u8; 32],
    });
    // Leading zero bytes of the target are a rough difficulty gauge.
    let difficulty_zero_bytes = tip_target.iter().take_while(|b| **b == 0).count();
    (
        StatusCode::OK,
        Json(json!({
            "height": height,
            "tip": hex::encode(tip_hash),
            "target": hex::encode(tip_target),
            "target_leading_zero_bytes": difficulty_zero_bytes,
        })),
    )
}
