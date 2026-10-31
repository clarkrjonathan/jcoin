//! Node core: the chain manager.
//!
//! The REST and P2P layers never touch node state directly. They send a
//! `NodeRequest` over an `mpsc` channel and wait on a per-request `oneshot`
//! reply. A single task (`run_node`) owns the chain, the tip state, and the
//! mempool, so there are no locks.

use std::collections::{HashMap, HashSet};

use tokio::sync::{mpsc, oneshot};

use crate::block::{Block, BlockHeader, BLOCK_REWARD, STATIC_TARGET};
use crate::state::State;
use crate::Transaction;

/// A request to the node core.
pub enum NodeRequest {
    /// Submit a transaction for validation, mempool admission, and relay.
    SubmitTransaction {
        tx: Transaction,
        reply: oneshot::Sender<SubmitResult>,
    },
    /// Ask whether the node has seen a transaction id.
    GetStatus {
        tx_id: String,
        reply: oneshot::Sender<StatusResult>,
    },
    /// Look up an account's balance and next nonce from the tip state.
    GetAccount {
        address: [u8; 32],
        reply: oneshot::Sender<(u64, u64)>,
    },
    /// Submit a block (mined locally or received by gossip).
    SubmitBlock {
        block: Box<Block>,
        reply: oneshot::Sender<BlockOutcome>,
    },
    /// Get a template for mining the next block: the tip header, the tip state,
    /// and an applicable mempool transaction if any.
    GetMiningJob {
        reply: oneshot::Sender<MiningJob>,
    },
    /// Chain height and tip hash.
    GetChainInfo {
        reply: oneshot::Sender<ChainInfo>,
    },
}

/// Outcome of submitting a transaction.
pub enum SubmitResult {
    /// Valid. `is_new` is true only the first time this node sees it; the caller
    /// gossips only when `is_new` is true, so a relayed transaction does not loop.
    Accepted { tx_id: String, is_new: bool },
    Rejected { reason: String },
}

/// Outcome of a status query.
pub enum StatusResult {
    Seen,
    Unknown,
}

/// Outcome of submitting a block.
pub enum BlockOutcome {
    /// Accepted and appended. `is_new` drives gossip.
    Accepted { is_new: bool, height: u64 },
    Rejected { reason: String },
    /// Does not extend our tip: we are missing ancestry or it is a fork. The
    /// chain-sync step handles catching up; for now such a block is not applied.
    Orphan,
}

/// A template for mining the next block.
pub struct MiningJob {
    pub parent: BlockHeader,
    pub parent_state: State,
    pub normal: Option<Transaction>,
}

/// Chain summary.
pub struct ChainInfo {
    pub height: u64,
    pub tip_hash: [u8; 32],
}

/// The node core task. Owns the chain, the tip state, and the mempool.
pub async fn run_node(mut rx: mpsc::Receiver<NodeRequest>) {
    let genesis = Block::genesis();
    let mut seen_block: HashSet<[u8; 32]> = HashSet::new();
    seen_block.insert(genesis.hash());

    let mut chain: Vec<Block> = vec![genesis];
    let mut state = State::new();
    let mut mempool: HashMap<String, Transaction> = HashMap::new();
    let mut seen_tx: HashSet<String> = HashSet::new();

    while let Some(request) = rx.recv().await {
        match request {
            NodeRequest::SubmitTransaction { tx, reply } => {
                let tx_id = tx.tx_id_hex();
                let result = validate_pending(&tx, &state).map_or_else(
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
                let _ = reply.send((state.balance(&address), state.nonce(&address)));
            }

            NodeRequest::SubmitBlock { block, reply } => {
                let outcome = accept_block(
                    *block,
                    &mut chain,
                    &mut state,
                    &mut mempool,
                    &mut seen_tx,
                    &mut seen_block,
                );
                let _ = reply.send(outcome);
            }

            NodeRequest::GetMiningJob { reply } => {
                let parent = chain.last().unwrap().header.clone();
                let normal = applicable_tx(&mempool, &state);
                let _ = reply.send(MiningJob {
                    parent,
                    parent_state: state.clone(),
                    normal,
                });
            }

            NodeRequest::GetChainInfo { reply } => {
                let tip = chain.last().unwrap();
                let _ = reply.send(ChainInfo {
                    height: tip.header.height,
                    tip_hash: tip.hash(),
                });
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

/// Validate a block against the tip and, if it extends the tip, append it.
fn accept_block(
    block: Block,
    chain: &mut Vec<Block>,
    state: &mut State,
    mempool: &mut HashMap<String, Transaction>,
    seen_tx: &mut HashSet<String>,
    seen_block: &mut HashSet<[u8; 32]>,
) -> BlockOutcome {
    let hash = block.hash();
    if seen_block.contains(&hash) {
        return BlockOutcome::Accepted {
            is_new: false,
            height: block.header.height,
        };
    }

    let tip = chain.last().unwrap();
    let extends_tip = block.header.height == tip.header.height + 1
        && block.header.previous_block_hash == tip.header.block_hash();
    if !extends_tip {
        // Ahead of us or on a fork; the chain-sync step resolves this.
        return BlockOutcome::Orphan;
    }

    match block.validate_and_apply(&tip.header, state, BLOCK_REWARD, &STATIC_TARGET) {
        Ok(new_state) => {
            *state = new_state;
            for tx in &block.transactions {
                let id = tx.tx_id_hex();
                seen_tx.insert(id.clone());
                mempool.remove(&id);
            }
            seen_block.insert(hash);
            let height = block.header.height;
            chain.push(block);
            BlockOutcome::Accepted {
                is_new: true,
                height,
            }
        }
        Err(e) => BlockOutcome::Rejected {
            reason: e.to_string(),
        },
    }
}
