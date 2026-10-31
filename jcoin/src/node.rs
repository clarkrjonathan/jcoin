//! Node core: the chain manager.
//!
//! The REST and P2P layers never touch node state directly. They send a
//! `NodeRequest` over an `mpsc` channel and wait on a per-request `oneshot`
//! reply. A single task (`run_node`) owns the chain store, the mempool, and the
//! transaction-seen set, so there are no locks.

use std::collections::{HashMap, HashSet};

use tokio::sync::{mpsc, oneshot};

use crate::block::{Block, BlockHeader};
use crate::chain::{Accept, Chain};
use crate::state::State;
use crate::Transaction;

/// Most blocks to return in one `BLOCKS` reply.
pub const BLOCK_BATCH: usize = 500;

/// A request to the node core.
pub enum NodeRequest {
    SubmitTransaction {
        tx: Transaction,
        reply: oneshot::Sender<SubmitResult>,
    },
    GetStatus {
        tx_id: String,
        reply: oneshot::Sender<StatusResult>,
    },
    GetAccount {
        address: [u8; 32],
        reply: oneshot::Sender<(u64, u64)>,
    },
    SubmitBlock {
        block: Box<Block>,
        reply: oneshot::Sender<BlockOutcome>,
    },
    GetMiningJob {
        reply: oneshot::Sender<MiningJob>,
    },
    GetChainInfo {
        reply: oneshot::Sender<ChainInfo>,
    },
    /// A block locator for the canonical tip (for syncing from a peer).
    GetLocator {
        reply: oneshot::Sender<Vec<[u8; 32]>>,
    },
    /// Canonical blocks after the most recent hash in `locator` we know.
    GetBlocksAfter {
        locator: Vec<[u8; 32]>,
        reply: oneshot::Sender<Vec<Block>>,
    },
}

pub enum SubmitResult {
    Accepted { tx_id: String, is_new: bool },
    Rejected { reason: String },
}

pub enum StatusResult {
    Seen,
    Unknown,
}

/// Outcome of submitting a block.
pub enum BlockOutcome {
    /// Stored and connected. `is_new` drives gossip; `new_tip` means it advanced
    /// the canonical chain.
    Accepted { is_new: bool, new_tip: bool, height: u64 },
    Rejected { reason: String },
    /// Parent unknown: the caller should sync the gap from that peer.
    Orphan,
}

pub struct MiningJob {
    pub parent: BlockHeader,
    pub parent_state: State,
    pub normal: Option<Transaction>,
}

pub struct ChainInfo {
    pub height: u64,
    pub tip_hash: [u8; 32],
}

/// The node core task.
pub async fn run_node(mut rx: mpsc::Receiver<NodeRequest>) {
    let mut chain = Chain::new();
    let mut mempool: HashMap<String, Transaction> = HashMap::new();
    let mut seen_tx: HashSet<String> = HashSet::new();

    while let Some(request) = rx.recv().await {
        match request {
            NodeRequest::SubmitTransaction { tx, reply } => {
                let tx_id = tx.tx_id_hex();
                let result = validate_pending(&tx, chain.tip_state()).map_or_else(
                    |reason| SubmitResult::Rejected { reason },
                    |()| {
                        let is_new = seen_tx.insert(tx_id.clone());
                        if is_new {
                            mempool.insert(tx_id.clone(), tx);
                        }
                        SubmitResult::Accepted { tx_id, is_new }
                    },
                );
                let _ = reply.send(result);
            }

            NodeRequest::GetStatus { tx_id, reply } => {
                let status = if seen_tx.contains(&tx_id) {
                    StatusResult::Seen
                } else {
                    StatusResult::Unknown
                };
                let _ = reply.send(status);
            }

            NodeRequest::GetAccount { address, reply } => {
                let state = chain.tip_state();
                let _ = reply.send((state.balance(&address), state.nonce(&address)));
            }

            NodeRequest::SubmitBlock { block, reply } => {
                let outcome = match chain.accept(*block) {
                    Accept::Stored { new_tip, height } => {
                        // Drop transactions no longer applicable on the new tip.
                        prune_mempool(&chain, &mut mempool, &mut seen_tx);
                        BlockOutcome::Accepted {
                            is_new: true,
                            new_tip,
                            height,
                        }
                    }
                    Accept::AlreadyHave { height } => BlockOutcome::Accepted {
                        is_new: false,
                        new_tip: false,
                        height,
                    },
                    Accept::Orphan => BlockOutcome::Orphan,
                    Accept::Invalid(e) => BlockOutcome::Rejected {
                        reason: e.to_string(),
                    },
                };
                let _ = reply.send(outcome);
            }

            NodeRequest::GetMiningJob { reply } => {
                let parent = chain.tip_header();
                let normal = applicable_tx(&mempool, chain.tip_state());
                let _ = reply.send(MiningJob {
                    parent,
                    parent_state: chain.tip_state().clone(),
                    normal,
                });
            }

            NodeRequest::GetChainInfo { reply } => {
                let _ = reply.send(ChainInfo {
                    height: chain.tip_height(),
                    tip_hash: chain.tip_hash(),
                });
            }

            NodeRequest::GetLocator { reply } => {
                let _ = reply.send(chain.locator());
            }

            NodeRequest::GetBlocksAfter { locator, reply } => {
                let _ = reply.send(chain.blocks_after(&locator, BLOCK_BATCH));
            }
        }
    }
}

/// Full validation of a loose transaction against the tip state (ADR 000: relay
/// only fully valid transactions). Returns `Err(reason)` if it should not relay.
fn validate_pending(tx: &Transaction, state: &State) -> Result<(), String> {
    if tx.is_coinbase() {
        return Err("coinbase cannot be submitted directly".to_string());
    }
    if !tx.verify_signature() {
        return Err("invalid signature".to_string());
    }
    let expected = state.nonce(&tx.sender);
    if tx.nonce != expected {
        return Err(format!("bad nonce: expected {expected}, got {}", tx.nonce));
    }
    let debit = tx
        .amount
        .checked_add(tx.fee)
        .ok_or_else(|| "amount + fee overflows".to_string())?;
    let balance = state.balance(&tx.sender);
    if balance < debit {
        return Err(format!("insufficient balance: need {debit}, have {balance}"));
    }
    Ok(())
}

/// Pick a mempool transaction that is currently applicable on top of the tip.
fn applicable_tx(mempool: &HashMap<String, Transaction>, state: &State) -> Option<Transaction> {
    mempool
        .values()
        .find(|tx| {
            tx.nonce == state.nonce(&tx.sender)
                && state.balance(&tx.sender) >= tx.amount.saturating_add(tx.fee)
        })
        .cloned()
}

/// Drop mempool transactions that are no longer applicable on the current tip
/// (already included, or stale nonce). Abandoned-branch transactions are not
/// re-added on a reorg yet; that is a later refinement.
fn prune_mempool(
    chain: &Chain,
    mempool: &mut HashMap<String, Transaction>,
    _seen_tx: &mut HashSet<String>,
) {
    let state = chain.tip_state();
    mempool.retain(|_id, tx| {
        tx.nonce == state.nonce(&tx.sender)
            && state.balance(&tx.sender) >= tx.amount.saturating_add(tx.fee)
    });
}
