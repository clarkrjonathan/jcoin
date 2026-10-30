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
    /// The transaction is valid. `is_new` is true only the first time this node
    /// sees it; the caller gossips a transaction only when `is_new` is true, so
    /// a relayed transaction does not loop forever.
    Accepted { tx_id: String, is_new: bool },
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
/// in-memory stand-in for the mempool, and it also provides gossip dedup: a
/// transaction is only new the first time it is inserted. Relaying is the
/// network layer's job, driven by the `is_new` flag returned here.
pub async fn run_node(mut rx: mpsc::Receiver<NodeRequest>) {
    let mut seen: HashSet<String> = HashSet::new();

    while let Some(request) = rx.recv().await {
        match request {
            NodeRequest::SubmitTransaction { tx, reply } => {
                let tx_id = tx.tx_id_hex();
                let result = if tx.verify_signature() {
                    let is_new = seen.insert(tx_id.clone());
                    SubmitResult::Accepted { tx_id, is_new }
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
