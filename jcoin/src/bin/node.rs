//! JeffyCoin node binary.
//!
//! Runs three things over one Tokio runtime: the node-core task (owns state),
//! an axum REST server (wallet-facing), and the P2P layer (a libp2p host:
//! TCP + Noise + Yamux, relay circuits with hole punching, and rendezvous
//! discovery, carrying the JeffyCoin protocol on one stream per peer).
//! Transactions that arrive by REST or by gossip are validated by the core
//! and, when new, relayed to the other peers.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
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
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};

use jcoin::block::{assemble_block, mine_chunk, Block, BlockError, BlockHeader};
use jcoin::Transaction;
use jcoin::consensus::{MAINNET, REGTEST};
use jcoin::node::{
    run_node, BlockOutcome, ChainInfo, MiningJob, NodeRequest, StatusResult, SubmitResult,
};
use jcoin::p2p::{load_or_create_keypair, parse_addr, run_host, HostConfig, Network};

#[derive(Parser)]
#[command(name = "jcoin-node", about = "JeffyCoin node")]
struct Cli {
    /// Address for the wallet-facing REST server.
    #[arg(long, default_value = "0.0.0.0:3000")]
    rest_addr: String,
    /// P2P listen address: `ip:port` (TCP) or a multiaddr.
    #[arg(long, default_value = "0.0.0.0:8333")]
    p2p_addr: String,
    /// Peer to dial on startup and redial while disconnected: `ip:port` or a
    /// multiaddr, optionally ending in `/p2p/<peer id>` (repeatable).
    #[arg(long = "peer")]
    peers: Vec<String>,
    /// Do not listen for inbound connections. The node then dials out only
    /// and, with --relay, is reachable only through the relay (as a node
    /// behind carrier-grade NAT would be).
    #[arg(long)]
    no_listen: bool,
    /// An extra reachable address of this node to advertise, such as a public
    /// address behind port forwarding (repeatable). Listen addresses are
    /// advertised automatically.
    #[arg(long = "advertise")]
    advertise: Vec<String>,
    /// Soft cap on peers before the auto-dialer stops reaching out.
    #[arg(long, default_value_t = 8)]
    max_peers: usize,
    /// A relay for nodes behind NAT, as a multiaddr ending in `/p2p/<relay
    /// peer id>` (the relay prints it at startup). The node reserves a slot on
    /// it, registers with its rendezvous service, and discovers other nodes
    /// there; hole punching upgrades relayed connections to direct ones when
    /// the NATs allow it.
    #[arg(long)]
    relay: Option<String>,
    /// File holding this node's libp2p identity key (created if missing).
    /// Defaults to `<data-dir>/node_key`; without a data dir the identity is
    /// new on every start.
    #[arg(long)]
    key_file: Option<std::path::PathBuf>,
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
    /// Block templates handed to external miners (see `mining_work`).
    work: Arc<Mutex<WorkStore>>,
}

/// Listen addresses for a node. If `addr` is the IPv4 unspecified address
/// (`0.0.0.0`), also listen on the IPv6 unspecified address (`[::]`) on the same
/// TCP port, so the node is dual-stack and reachable over IPv6 where both ends
/// have it — bypassing IPv4 carrier-grade NAT (ADR 0003). A concrete address is
/// left as the sole listener (the operator asked for exactly that).
fn dual_stack(addr: libp2p::Multiaddr) -> Vec<libp2p::Multiaddr> {
    use libp2p::multiaddr::Protocol;
    use std::net::{Ipv4Addr, Ipv6Addr};
    let parts: Vec<_> = addr.iter().collect();
    if let (Some(Protocol::Ip4(ip)), Some(Protocol::Tcp(port))) = (parts.first(), parts.get(1)) {
        if *ip == Ipv4Addr::UNSPECIFIED {
            let v6 = libp2p::Multiaddr::empty()
                .with(Protocol::Ip6(Ipv6Addr::UNSPECIFIED))
                .with(Protocol::Tcp(*port));
            return vec![addr, v6];
        }
    }
    vec![addr]
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let listen = parse_addr(&cli.p2p_addr).expect("bad --p2p-addr");
    let listen_port = listen
        .iter()
        .find_map(|p| match p {
            libp2p::multiaddr::Protocol::Tcp(port) => Some(port),
            _ => None,
        })
        .unwrap_or(0);

    // Node core.
    let (node_tx, node_rx) = mpsc::channel::<NodeRequest>(100);
    let params = if cli.regtest { REGTEST } else { MAINNET };
    tokio::spawn(run_node(node_rx, cli.data_dir.clone(), params));

    let network = Network::new(node_tx.clone(), listen_port, Vec::new(), cli.max_peers);

    // P2P host: identity, listener, bootstrap peers, and the optional relay.
    let key_file = cli
        .key_file
        .clone()
        .or_else(|| cli.data_dir.as_ref().map(|d| d.join("node_key")));
    let keypair = match &key_file {
        Some(path) => load_or_create_keypair(path).expect("cannot load or create the node key"),
        None => libp2p::identity::Keypair::generate_ed25519(),
    };
    let parse_all = |list: &[String], flag: &str| -> Vec<libp2p::Multiaddr> {
        list.iter()
            .map(|s| parse_addr(s).unwrap_or_else(|e| panic!("bad {flag}: {e}")))
            .collect()
    };
    let config = HostConfig {
        keypair,
        listen: if cli.no_listen {
            Vec::new()
        } else {
            dual_stack(listen)
        },
        advertise: parse_all(&cli.advertise, "--advertise"),
        bootstrap: parse_all(&cli.peers, "--peer"),
        relay: cli
            .relay
            .as_deref()
            .map(|s| parse_addr(s).expect("bad --relay")),
        tick: Duration::from_secs(10),
    };
    {
        let network = network.clone();
        tokio::spawn(async move {
            if let Err(e) = run_host(config, network).await {
                eprintln!("[p2p] host stopped: {e}");
            }
        });
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
        work: Arc::new(Mutex::new(WorkStore::default())),
    };

    let app = Router::new()
        .route("/wallet/transaction", post(submit_transaction))
        .route("/wallet/transaction/{tx_id}", get(transaction_status))
        .route("/balance/{address}", get(balance))
        .route("/chain", get(chain_info))
        .route("/mining/work/{miner}", get(mining_work))
        .route("/mining/submit", post(mining_submit))
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

/// Mine blocks continuously, crediting `miner`. Each round asks the core for a
/// job (tip + state + an applicable mempool transaction), mines off the runtime
/// with `spawn_blocking`, submits the result, and gossips it when accepted. A
/// short pace between rounds keeps a fast static target from flooding blocks.
async fn mine_loop(network: Network, miner: [u8; 32]) {
    // Nonces tried per blocking burst before checking whether to keep mining.
    const CHUNK: u64 = 200_000;

    loop {
        let Template {
            mut header,
            transactions,
        } = match new_template(network.node(), miner).await {
            None => return,
            Some(Ok(t)) => t,
            Some(Err(e)) => {
                eprintln!("[mine] assemble failed: {e}");
                tokio::time::sleep(Duration::from_millis(500)).await;
                continue;
            }
        };
        let target_height = header.height;

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
                if submit_mined(&network, block, "mine").await.is_none() {
                    return;
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

/// A block ready to mine: a header (nonce 0) and the transactions it commits to.
#[derive(Clone)]
struct Template {
    header: BlockHeader,
    transactions: Vec<Transaction>,
}

/// Ask the core for a mining job (tip, its state, an applicable mempool
/// transaction) and assemble a block template that pays `miner`. `None` means
/// the core is gone; `Some(Err)` means assembly failed.
async fn new_template(
    node: &mpsc::Sender<NodeRequest>,
    miner: [u8; 32],
) -> Option<Result<Template, BlockError>> {
    let (job_tx, job_rx) = oneshot::channel();
    node.send(NodeRequest::GetMiningJob { reply: job_tx })
        .await
        .ok()?;
    let MiningJob {
        parent,
        parent_state,
        normal,
        context,
    } = job_rx.await.ok()?;

    // Never below the consensus minimum: a clock behind median-time-past
    // would otherwise build blocks the chain rejects, forever.
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let timestamp = now.max(context.min_timestamp);
    Some(
        assemble_block(
            &parent,
            &parent_state,
            context.reward,
            miner,
            normal,
            context.target,
            timestamp,
        )
        .map(|(mut header, transactions, _state)| {
            // Stamp the height-selected consensus version, not the binary's
            // constant, so mining uses the same per-block input as validation
            // (consensus inputs all flow through BlockContext).
            header.version = context.version;
            Template {
                header,
                transactions,
            }
        }),
    )
}

/// Submit a mined block to the core and gossip it when it is new. Logs the
/// outcome under `tag`. `None` means the core is gone.
async fn submit_mined(network: &Network, block: Block, tag: &str) -> Option<BlockOutcome> {
    println!(
        "[{tag}] found candidate height {} ({} txs) {}",
        block.header.height,
        block.transactions.len(),
        hex::encode(block.hash())
    );
    let (ack_tx, ack_rx) = oneshot::channel();
    network
        .node()
        .send(NodeRequest::SubmitBlock {
            block: Box::new(block.clone()),
            reply: ack_tx,
        })
        .await
        .ok()?;
    let outcome = ack_rx.await.ok()?;
    match &outcome {
        BlockOutcome::Accepted { is_new: true, height, .. } => {
            println!("[{tag}] won block {height} {}", hex::encode(block.hash()));
            network.broadcast_block(&block, None);
        }
        BlockOutcome::Accepted { is_new: false, height, .. } => {
            println!("[{tag}] candidate at height {height} already known");
        }
        BlockOutcome::Rejected { reason } => {
            println!("[{tag}] candidate REJECTED: {reason}");
        }
        BlockOutcome::Orphan => {
            println!("[{tag}] candidate ORPHAN (parent unknown)");
        }
    }
    Some(outcome)
}

/// Templates handed to external miners by GET /mining/work, kept so that a
/// submitted nonce can be matched to its template. Bounded: the oldest are
/// dropped, and a nonce for a dropped template is reported as stale.
#[derive(Default)]
struct WorkStore {
    next_id: u64,
    jobs: VecDeque<(u64, Template)>,
}

const MAX_WORK: usize = 64;

/// GET /mining/work/{miner}
///
/// A block template for an external miner (for example the GPU miner) that
/// pays the hex address `miner`. Returns the job id, the height, the 156-byte
/// header with nonce 0 (hex), and the target (hex). The miner searches the
/// nonce (the last 8 header bytes, big-endian) for SHA-256(header) < target
/// and posts the job id and nonce to /mining/submit. Fetch new work when the
/// chain height changes.
async fn mining_work(
    State(state): State<AppState>,
    Path(miner): Path<String>,
) -> impl IntoResponse {
    let miner: [u8; 32] = match hex::decode(&miner).ok().and_then(|b| b.try_into().ok()) {
        Some(m) => m,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(json!({ "error": "miner must be a 32-byte hex address" })),
            )
        }
    };
    let template = match new_template(&state.node, miner).await {
        Some(Ok(t)) => t,
        Some(Err(e)) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": format!("assemble failed: {e}") })),
            )
        }
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "node core unavailable" })),
            )
        }
    };
    let body = |id: u64, t: &Template| {
        json!({
            "job": id,
            "height": t.header.height,
            "header": hex::encode(t.header.serialize()),
            "target": hex::encode(t.header.target),
        })
    };
    let mut store = state.work.lock().unwrap();
    let id = store.next_id;
    store.next_id += 1;
    let response = body(id, &template);
    store.jobs.push_back((id, template));
    while store.jobs.len() > MAX_WORK {
        store.jobs.pop_front();
    }
    (StatusCode::OK, Json(response))
}

#[derive(serde::Deserialize)]
struct WorkSubmission {
    job: u64,
    nonce: u64,
}

/// POST /mining/submit  `{"job": <id>, "nonce": <u64>}`
///
/// Puts the nonce into the job's template and submits the block. `status` is
/// `accepted` (new tip, gossiped), `known`, `rejected` (with `reason`),
/// `orphan`, `stale` (job no longer held), or `invalid` (hash does not meet
/// the target; HTTP 400).
async fn mining_submit(
    State(state): State<AppState>,
    Json(sub): Json<WorkSubmission>,
) -> impl IntoResponse {
    let template = {
        let store = state.work.lock().unwrap();
        store
            .jobs
            .iter()
            .find(|(id, _)| *id == sub.job)
            .map(|(_, t)| t.clone())
    };
    let Some(Template {
        mut header,
        transactions,
    }) = template
    else {
        return (StatusCode::OK, Json(json!({ "status": "stale" })));
    };
    header.nonce = sub.nonce;
    if !header.meets_target() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "status": "invalid", "error": "hash does not meet the target" })),
        );
    }
    let block = Block {
        header,
        transactions,
    };
    let hash = hex::encode(block.hash());
    let body = match submit_mined(&state.network, block, "work").await {
        Some(BlockOutcome::Accepted { is_new: true, height, .. }) => {
            json!({ "status": "accepted", "height": height, "hash": hash })
        }
        Some(BlockOutcome::Accepted { is_new: false, height, .. }) => {
            json!({ "status": "known", "height": height, "hash": hash })
        }
        Some(BlockOutcome::Rejected { reason }) => {
            json!({ "status": "rejected", "reason": reason.to_string() })
        }
        Some(BlockOutcome::Orphan) => json!({ "status": "orphan" }),
        None => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": "node core unavailable" })),
            )
        }
    };
    (StatusCode::OK, Json(body))
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

/// GET /balance/{address} — the account's confirmed balance and nonce at the
/// tip, plus its pending chain: `next_nonce` (the nonce a new transaction must
/// use) and `pending` (base units already committed by pending transactions).
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

    // A missing reply is an error, never a zero balance and nonce: a wallet
    // would sign with nonce 0 and the transaction would be rejected.
    let Ok(info) = reply_rx.await else {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "no response from node core" })),
        );
    };
    (
        StatusCode::OK,
        Json(json!({
            "address": address,
            "balance": info.balance,
            "nonce": info.nonce,
            "next_nonce": info.next_nonce,
            "pending": info.pending_debit,
        })),
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
