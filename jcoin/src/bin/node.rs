//! JeffyCoin node binary for the initial vertical.
//!
//! Runs an axum REST server and a separate node-core task. REST handlers talk to
//! the core over an `mpsc` channel and await a `oneshot` reply; they never touch
//! node state directly.

use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde_json::json;
use tokio::sync::{mpsc, oneshot};

use jcoin::node::{run_node, NodeRequest, StatusResult, SubmitResult};
use jcoin::Transaction;

/// Shared state handed to every request handler: a sender into the node core.
#[derive(Clone)]
struct AppState {
    node: mpsc::Sender<NodeRequest>,
}

#[tokio::main]
async fn main() {
    // Channel from the REST layer to the single node-core task.
    let (node_tx, node_rx) = mpsc::channel::<NodeRequest>(100);
    tokio::spawn(run_node(node_rx));

    let state = AppState { node: node_tx };

    let app = Router::new()
        .route("/wallet/transaction", post(submit_transaction))
        .route("/wallet/transaction/{tx_id}", get(transaction_status))
        .route("/balance/{address}", get(balance))
        .with_state(state);

    let addr = std::env::var("JCOIN_NODE_ADDR").unwrap_or_else(|_| "0.0.0.0:3000".to_string());
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("failed to bind node address");

    println!("JeffyCoin node listening on {addr}");
    axum::serve(listener, app)
        .await
        .expect("server error");
}

/// POST /wallet/transaction
///
/// A malformed body is rejected by the `Json<Transaction>` extractor with 400
/// before this handler runs. A well-formed but invalid transaction (bad
/// signature) is not an HTTP error: it returns 200 with `status: rejected`, so
/// the wallet polls status rather than treating it as a transport failure.
async fn submit_transaction(
    State(state): State<AppState>,
    Json(tx): Json<Transaction>,
) -> impl IntoResponse {
    let (reply_tx, reply_rx) = oneshot::channel();

    if state
        .node
        .send(NodeRequest::SubmitTransaction { tx, reply: reply_tx })
        .await
        .is_err()
    {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({ "error": "node core unavailable" })),
        );
    }

    match reply_rx.await {
        Ok(SubmitResult::Accepted { tx_id }) => (
            StatusCode::OK,
            Json(json!({ "status": "accepted", "relayed": true, "tx_id": tx_id })),
        ),
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

/// GET /wallet/transaction/{tx_id}
///
/// Dummy status for the vertical: reports whether the node has seen the id.
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

/// GET /balance/{address}
///
/// Dummy for the vertical: always returns 0. Balances arrive with the state model.
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
