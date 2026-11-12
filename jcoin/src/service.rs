//! The JeffyCoin node as a service that a program can embed.
//!
//! `run` starts three things on the current Tokio runtime: the node-core task
//! (owns state), an axum REST server (wallet-facing), and the P2P layer (a
//! libp2p host: TCP + Noise + Yamux, relay circuits with hole punching, and
//! rendezvous discovery, carrying the JeffyCoin protocol on one stream per
//! peer). Transactions that arrive by REST or by gossip are validated by the
//! core and, when new, relayed to the other peers.
//!
//! The `node` binary is a command-line front end for this module; the desktop
//! app embeds it in its own process.

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
use serde_json::json;
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};

use crate::block::{assemble_block, mine_chunk, Block, BlockError, BlockHeader};
use crate::consensus::{MAINNET, REGTEST};
use crate::node::{
    run_node, BlockOutcome, ChainInfo, MiningJob, NodeRequest, StatusResult, SubmitResult,
};
use crate::p2p::{load_or_create_keypair, parse_addr, run_host, HostConfig, Network};
use crate::Transaction;

/// How to run a node. The fields mirror the `node` binary's flags.
#[derive(Debug, Clone)]
pub struct ServiceConfig {
    /// Address for the wallet-facing REST server.
    pub rest_addr: String,
    /// P2P listen address: `ip:port` (TCP) or a multiaddr.
    pub p2p_addr: String,
    /// Peers to dial on startup and redial while disconnected.
    pub peers: Vec<String>,
    /// Do not listen for inbound connections (dial out and use the relay only).
    pub no_listen: bool,
    /// Extra reachable addresses of this node to advertise.
    pub advertise: Vec<String>,
    /// Soft cap on peers before the auto-dialer stops reaching out.
    pub max_peers: usize,
    /// A relay for nodes behind NAT, as a multiaddr ending in `/p2p/<peer id>`.
    pub relay: Option<String>,
    /// File holding the libp2p identity key. Defaults to `<data_dir>/node_key`.
    pub key_file: Option<std::path::PathBuf>,
    /// Address that receives coinbase rewards from the built-in CPU miner.
    /// `None` runs no CPU miner (external miners use the mining endpoints).
    pub cpu_miner: Option<[u8; 32]>,
    /// Directory to persist the chain in. `None` runs in memory only.
    pub data_dir: Option<std::path::PathBuf>,
    /// Run on the local test network instead of the real one.
    pub regtest: bool,
    /// A friendly label for this node, sent to peers after the handshake and
    /// shown in their connections list. `None` sends nothing.
    pub label: Option<String>,
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

/// Run the node until the REST server stops. Returns an error for a bad
/// address, an unusable identity key, or a REST address that cannot be bound.
pub async fn run(config: ServiceConfig) -> Result<(), String> {
    let listen = parse_addr(&config.p2p_addr).map_err(|e| format!("bad P2P address: {e}"))?;
    let listen_port = listen
        .iter()
        .find_map(|p| match p {
            libp2p::multiaddr::Protocol::Tcp(port) => Some(port),
            _ => None,
        })
        .unwrap_or(0);

    // Node core.
    let (node_tx, node_rx) = mpsc::channel::<NodeRequest>(100);
    let params = if config.regtest { REGTEST } else { MAINNET };
    tokio::spawn(run_node(node_rx, config.data_dir.clone(), params));

    let network = Network::new(node_tx.clone(), listen_port, Vec::new(), config.max_peers);
    network.set_own_label(config.label.clone());

    // P2P host: identity, listener, bootstrap peers, and the optional relay.
    let key_file = config
        .key_file
        .clone()
        .or_else(|| config.data_dir.as_ref().map(|d| d.join("node_key")));
    let keypair = match &key_file {
        Some(path) => load_or_create_keypair(path)
            .map_err(|e| format!("cannot load or create the node key: {e}"))?,
        None => libp2p::identity::Keypair::generate_ed25519(),
    };
    let parse_all = |list: &[String], what: &str| -> Result<Vec<libp2p::Multiaddr>, String> {
        list.iter()
            .map(|s| parse_addr(s).map_err(|e| format!("bad {what} address: {e}")))
            .collect()
    };
    let host = HostConfig {
        keypair,
        listen: if config.no_listen {
            Vec::new()
        } else {
            dual_stack(listen)
        },
        advertise: parse_all(&config.advertise, "advertise")?,
        bootstrap: parse_all(&config.peers, "peer")?,
        relay: match config.relay.as_deref() {
            Some(s) => Some(parse_addr(s).map_err(|e| format!("bad relay address: {e}"))?),
            None => None,
        },
        tick: Duration::from_secs(10),
    };
    {
        let network = network.clone();
        tokio::spawn(async move {
            if let Err(e) = run_host(host, network).await {
                crate::log_error!("[p2p] host stopped: {e}");
            }
        });
    }

    // Optional CPU miner.
    if let Some(miner) = config.cpu_miner {
        tokio::spawn(mine_loop(network.clone(), miner));
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
        .route("/peers", get(peers))
        .route("/network/signal", get(network_signal))
        .route("/mining/work/{miner}", get(mining_work))
        .route("/mining/submit", post(mining_submit))
        .with_state(state);

    let listener = TcpListener::bind(&config.rest_addr)
        .await
        .map_err(|e| format!("cannot bind the REST address {}: {e}", config.rest_addr))?;

    crate::log_info!(
        "JeffyCoin node: REST on {}, P2P on {}",
        config.rest_addr, config.p2p_addr
    );
    axum::serve(listener, app)
        .await
        .map_err(|e| format!("REST server error: {e}"))
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
                crate::log_error!("[mine] assemble failed: {e}");
                tokio::time::sleep(Duration::from_millis(500)).await;
                continue;
            }
        };
        let target_height = header.height;
        let started = std::time::Instant::now();

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
            // A template's timestamp fixes its target. Take a new one now and
            // then, so pending transfers are picked up and, when blocks are
            // slow, the stall fallback's easier target is used.
            if started.elapsed() >= TEMPLATE_MAX_AGE {
                break;
            }
        }
    }
}

/// How long the built-in miner works on one template before taking a new one.
const TEMPLATE_MAX_AGE: Duration = Duration::from_secs(20);

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
            // The stall fallback makes the required target depend on the
            // block's own timestamp.
            context.target_at(timestamp),
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
    crate::log_info!(
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
            crate::log_info!(
                [("height", height.to_string())];
                "[{tag}] won block {height} {}", hex::encode(block.hash())
            );
            network.broadcast_block(&block, None);
        }
        BlockOutcome::Accepted { is_new: false, height, .. } => {
            crate::log_info!("[{tag}] candidate at height {height} already known");
        }
        BlockOutcome::Rejected { reason } => {
            crate::log_info!("[{tag}] candidate REJECTED: {reason}");
        }
        BlockOutcome::Orphan => {
            crate::log_info!("[{tag}] candidate ORPHAN (parent unknown)");
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
    // Details of the block for the miner's own record: what the coinbase pays
    // (subsidy plus fees), how many transfers it carries, and its target.
    let reward = block.transactions.first().map(|tx| tx.amount).unwrap_or(0);
    let transfers = block.transactions.len().saturating_sub(1);
    let target = hex::encode(block.header.target);
    let timestamp = block.header.timestamp;
    let body = match submit_mined(&state.network, block, "work").await {
        Some(BlockOutcome::Accepted { is_new: true, height, .. }) => {
            json!({
                "status": "accepted",
                "height": height,
                "hash": hash,
                "reward": reward,
                "transfers": transfers,
                "target": target,
                "timestamp": timestamp,
            })
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

/// GET /network/signal
///
/// Whether blocks with a newer version than this build knows have been seen,
/// and how their work compares with this node's own chain (see `signal`).
async fn network_signal(State(state): State<AppState>) -> impl IntoResponse {
    let (reply_tx, reply_rx) = oneshot::channel();
    if state
        .node
        .send(NodeRequest::GetSignal { reply: reply_tx })
        .await
        .is_err()
    {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "node core unavailable" })),
        );
    }
    match reply_rx.await {
        Ok(s) => (
            StatusCode::OK,
            Json(json!({
                "level": s.level.as_str(),
                "known_version": crate::consensus::LATEST_VERSION,
                "newest_version": s.newest_version,
                "blocks": s.blocks,
                "work_ratio": s.work_ratio,
                "window_hours": crate::signal::WINDOW / 3600,
            })),
        ),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "no response from node core" })),
        ),
    }
}

/// GET /peers
///
/// This node's own addresses and its open connections, for diagnostics.
async fn peers(State(state): State<AppState>) -> impl IntoResponse {
    let connections: Vec<_> = state
        .network
        .connections()
        .into_iter()
        .map(|c| {
            json!({
                "peer": c.peer,
                "address": c.address,
                "outbound": c.outbound,
                "relayed": c.relayed,
                "is_relay": c.is_relay,
                "session": c.session,
                "since": c.since,
                "label": c.label,
            })
        })
        .collect();
    let advertised: Vec<String> = state
        .network
        .advertised()
        .iter()
        .map(|a| a.to_string())
        .collect();
    Json(json!({ "advertised": advertised, "connections": connections }))
}

/// POST /wallet/transaction
///
/// A malformed body is rejected by the extractor with 400 before this runs. A
/// valid but unaccepted transaction returns 200 with `status: rejected`. A newly
/// accepted transaction is gossiped to all peers.
async fn submit_transaction(
    State(state): State<AppState>,
    Json(tx): Json<crate::Transaction>,
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
            "peers": state.network.peer_count(),
        })),
    )
}
