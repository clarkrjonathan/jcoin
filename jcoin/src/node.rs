//! Node core for the initial vertical.
//!
//! The REST layer never touches node state directly. It sends a `NodeRequest`
//! over an `mpsc` channel and waits on a per-request `oneshot` reply. A single
//! task (`run_node`) owns all node state, so there are no locks. This is the
//! channel pattern described in the JeffyCoin spec notes.

use std::collections::HashSet;

use tokio::sync::{mpsc, oneshot};

use crate::Transaction;

/// A request from the REST layer (or, later, the P2P layer) to the node core.
pub enum NodeRequest {
    /// Submit a transaction for validation and relay.
    SubmitTransaction {
        tx: Transaction,
        reply: oneshot::Sender<SubmitResult>,
    },
    /// Ask whether the node has seen a transaction id.
    GetStatus {
        tx_id: String,
        reply: oneshot::Sender<StatusResult>,
    },
    /// Dummy balance lookup for the vertical.
    GetBalance {
        address: String,
        reply: oneshot::Sender<u64>,
    },
}

/// Outcome of submitting a transaction.
pub enum SubmitResult {
    Accepted { tx_id: String },
    Rejected { reason: String },
}

/// Outcome of a status query.
pub enum StatusResult {
    Seen,
    Unknown,
}

/// The node core task. Owns all node state and processes requests one at a time.
///
/// For the initial vertical the only validation is the Ed25519 signature. Nonce
/// and balance checks are intentionally not performed. The "seen" set is an
/// in-memory stand-in for the mempool.
pub async fn run_node(mut rx: mpsc::Receiver<NodeRequest>) {
    let mut seen: HashSet<String> = HashSet::new();

    while let Some(request) = rx.recv().await {
        match request {
            NodeRequest::SubmitTransaction { tx, reply } => {
                let tx_id = tx.tx_id_hex();
                let result = if tx.verify_signature() {
                    seen.insert(tx_id.clone());
                    relay_handler(&tx);
                    SubmitResult::Accepted { tx_id }
                } else {
                    SubmitResult::Rejected {
                        reason: "invalid signature".to_string(),
                    }
                };
                // The receiver may have dropped if the client disconnected.
                let _ = reply.send(result);
            }

            NodeRequest::GetStatus { tx_id, reply } => {
                let status = if seen.contains(&tx_id) {
                    StatusResult::Seen
                } else {
                    StatusResult::Unknown
                };
                let _ = reply.send(status);
            }

            NodeRequest::GetBalance { address: _, reply } => {
                // Dummy for the vertical: state/balances are not tracked yet.
                let _ = reply.send(0);
            }
        }
    }
}

/// The relay handler ("listener type" in the design notes). In a later phase it
/// gossips the transaction to connected peers over TCP. For the vertical it only
/// logs, since no peers are connected.
fn relay_handler(tx: &Transaction) {
    println!(
        "[relay] signature valid for tx {} — would relay to peers (none connected yet)",
        tx.tx_id_hex()
    );
}
