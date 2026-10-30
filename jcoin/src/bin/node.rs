//! JeffyCoin node binary.
//!
//! Runs three things over one Tokio runtime: the node-core task (owns state),
//! an axum REST server (wallet-facing), and the P2P layer (a TCP listener plus
//! a dialer per configured peer). Transactions that arrive by REST or by gossip
//! are validated by the core and, when new, relayed to the other peers.

use std::net::SocketAddr;
use std::time::Duration;

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

use jcoin::node::{run_node, NodeRequest, StatusResult, SubmitResult};
use jcoin::p2p::{run_peer, Network};

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
    tokio::spawn(run_node(node_rx));

    let network = Network::new();

    // P2P listener.
    tokio::spawn(p2p_listen(
        p2p_sock,
        listen_port,
        node_tx.clone(),
        network.clone(),
    ));

    // One dialer per configured peer; each retries on disconnect.
    for peer in cli.peers {
        tokio::spawn(p2p_dial(
            peer,
            listen_port,
            node_tx.clone(),
            network.clone(),
        ));
    }

    let state = AppState {
        node: node_tx,
        network,
    };

    let app = Router::new()
        .route("/wallet/transaction", post(submit_transaction))
        .route("/wallet/transaction/{tx_id}", get(transaction_status))
        .route("/balance/{address}", get(balance))
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
async fn p2p_listen(
    addr: SocketAddr,
    listen_port: u16,
    node: mpsc::Sender<NodeRequest>,
    network: Network,
) {
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
                    listen_port,
                    node.clone(),
                    network.clone(),
                    format!("{peer_addr} (in)"),
                ));
            }
            Err(e) => eprintln!("[p2p] accept error: {e}"),
        }
    }
}

/// Dial a peer and run it; on disconnect, wait and retry.
async fn p2p_dial(
    addr: String,
    listen_port: u16,
    node: mpsc::Sender<NodeRequest>,
    network: Network,
) {
    loop {
        match TcpStream::connect(&addr).await {
            Ok(stream) => {
                run_peer(
                    stream,
                    listen_port,
                    node.clone(),
                    network.clone(),
                    format!("{addr} (out)"),
                )
                .await;
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

/// GET /wallet/transaction/{tx_id} — dummy status: has the node seen the id.
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

/// GET /balance/{address} — dummy for the vertical: always 0.
async fn balance(State(state): State<AppState>, Path(address): Path<String>) -> impl IntoResponse {
    let (reply_tx, reply_rx) = oneshot::channel();
    let _ = state
        .node
        .send(NodeRequest::GetBalance {
            address: address.clone(),
            reply: reply_tx,
        })
        .await;

    let bal = reply_rx.await.unwrap_or(0);
    (
        StatusCode::OK,
        Json(json!({
            "address": address,
            "balance": bal,
            "note": "dummy value for the initial vertical"
        })),
    )
}
