//! Node core: the chain manager.
//!
//! The REST and P2P layers never touch node state directly. They send a
//! `NodeRequest` over an `mpsc` channel and wait on a per-request `oneshot`
//! reply. A single task (`run_node`) owns the chain store, the mempool, and the
//! seen-transaction cache, so there are no locks.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::sync::{mpsc, oneshot};

use crate::block::{Block, BlockHeader, MAX_FUTURE_DRIFT};
use crate::chain::{Accept, Chain};
use crate::consensus::{BlockContext, Params};
use crate::mempool::{Admitted, Mempool, SeenCache, MAX_MEMPOOL, SEEN_CAPACITY};
use crate::state::State;
use crate::store::{load_and_repair, BlockLog};
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
    /// In this node's mempool now.
    Pending,
    /// Seen recently but no longer pending (for example, included in a block,
    /// or dropped as no longer valid).
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
    /// What consensus requires of the block (height, reward, target, minimum
    /// timestamp), from the same function that validates it.
    pub context: BlockContext,
}

pub struct ChainInfo {
    pub height: u64,
    pub tip_hash: [u8; 32],
    /// The difficulty target of the current tip (lets clients see retargeting).
    pub tip_target: [u8; 32],
}

/// The node core task.
///
/// If `data_dir` is set, the chain is persisted to an append-only block log
/// there (`blocks.dat`): the log is replayed on startup to rebuild the chain and
/// state, and each newly accepted block is appended. With `None`, the node is
/// in-memory only. `params` selects the network (`MAINNET` or `REGTEST`).
pub async fn run_node(
    mut rx: mpsc::Receiver<NodeRequest>,
    data_dir: Option<PathBuf>,
    params: Params,
) {
    let mut chain = Chain::new(params);
    let mut mempool = Mempool::new(MAX_MEMPOOL);
    let mut seen_tx = SeenCache::new(SEEN_CAPACITY);

    // Replay persisted blocks (if any) and open the log for appending.
    let mut log: Option<BlockLog> = None;
    if let Some(dir) = data_dir {
        let path = dir.join("blocks.dat");
        match load_and_repair(&path) {
            Ok(loaded) => {
                if loaded.discarded > 0 {
                    eprintln!(
                        "[store] cut {} bytes of torn or corrupt tail from {}",
                        loaded.discarded,
                        path.display()
                    );
                }
                let count = loaded.blocks.len();
                let mut rejected = 0usize;
                for block in loaded.blocks {
                    match chain.accept(block) {
                        Accept::Stored { .. } | Accept::AlreadyHave { .. } => {}
                        Accept::Orphan => rejected += 1,
                        Accept::Invalid(e) => {
                            rejected += 1;
                            if rejected == 1 {
                                eprintln!("[store] replay rejected a block: {e}");
                            }
                        }
                    }
                }
                if rejected > 0 {
                    eprintln!(
                        "[store] replay rejected {rejected} of {count} blocks \
                         (consensus rules changed since they were written?)"
                    );
                }
                if count > 0 {
                    println!("[store] replayed {count} blocks; tip height {}", chain.tip_height());
                }
            }
            Err(e) => eprintln!("[store] failed to read {}: {e}", path.display()),
        }
        match BlockLog::open(&path) {
            Ok(l) => log = Some(l),
            Err(e) => eprintln!("[store] failed to open {} for append: {e}", path.display()),
        }
    }

    while let Some(request) = rx.recv().await {
        match request {
            NodeRequest::SubmitTransaction { tx, reply } => {
                let id = tx.tx_id();
                let tx_id = hex::encode(id);
                // Mempool membership, not the seen cache, decides whether this
                // is new: a transaction that left the mempool (for example after
                // a reorg) is admitted and relayed again when resubmitted.
                let result = match validate_pending(&tx, chain.tip_state()) {
                    Err(reason) => SubmitResult::Rejected { reason },
                    Ok(()) => match mempool.insert(tx) {
                        Ok(admitted) => {
                            seen_tx.insert(id);
                            SubmitResult::Accepted {
                                tx_id,
                                is_new: admitted == Admitted::New,
                            }
                        }
                        Err(e) => SubmitResult::Rejected {
                            reason: e.to_string(),
                        },
                    },
                };
                let _ = reply.send(result);
            }

            NodeRequest::GetStatus { tx_id, reply } => {
                let parsed: Option<[u8; 32]> =
                    hex::decode(&tx_id).ok().and_then(|b| b.try_into().ok());
                let status = match parsed {
                    Some(id) if mempool.contains(&id) => StatusResult::Pending,
                    Some(id) if seen_tx.contains(&id) => StatusResult::Seen,
                    _ => StatusResult::Unknown,
                };
                let _ = reply.send(status);
            }

            NodeRequest::GetAccount { address, reply } => {
                let state = chain.tip_state();
                let _ = reply.send((state.balance(&address), state.nonce(&address)));
            }

            NodeRequest::SubmitBlock { block, reply } => {
                // Local future-time bound (not consensus): reject a block more
                // than MAX_FUTURE_DRIFT ahead of our clock. Enforced here on
                // receipt, never inside chain.accept, so replay stays deterministic.
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                if block.header.timestamp > now.saturating_add(MAX_FUTURE_DRIFT) {
                    let _ = reply.send(BlockOutcome::Rejected {
                        reason: "timestamp too far in the future".to_string(),
                    });
                    continue;
                }

                // Clone for the log only when persisting.
                let for_log = log.as_ref().map(|_| (*block).clone());
                let outcome = match chain.accept(*block) {
                    Accept::Stored { new_tip, height } => {
                        if let (Some(log), Some(block)) = (log.as_mut(), for_log.as_ref()) {
                            if let Err(e) = log.append(block) {
                                eprintln!("[store] append failed: {e}");
                            }
                        }
                        // Drop transactions no longer applicable on the new tip.
                        mempool.prune(chain.tip_state());
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
                let normal = mempool.best_applicable(chain.tip_state());
                let _ = reply.send(MiningJob {
                    parent,
                    parent_state: chain.tip_state().clone(),
                    normal,
                    context: chain.next_context(),
                });
            }

            NodeRequest::GetChainInfo { reply } => {
                let _ = reply.send(ChainInfo {
                    height: chain.tip_height(),
                    tip_hash: chain.tip_hash(),
                    tip_target: chain.tip_header().target,
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
    if !tx.is_normal() {
        return Err(format!("unknown transaction type {}", tx.tx_type));
    }
    if !tx.verify_signature() {
        return Err("invalid signature".to_string());
    }
    state.check(tx).map(|_| ()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::{build_and_mine, Block};
    use crate::consensus::REGTEST;
    use crate::state::State;

    fn temp_dir(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("jcoin-node-test-{}-{}", std::process::id(), name));
        let _ = std::fs::remove_dir_all(&p);
        p
    }

    async fn height(core: &mpsc::Sender<NodeRequest>) -> u64 {
        let (reply, rx) = oneshot::channel();
        core.send(NodeRequest::GetChainInfo { reply }).await.unwrap();
        rx.await.unwrap().height
    }

    async fn get_job(core: &mpsc::Sender<NodeRequest>) -> MiningJob {
        let (reply, rx) = oneshot::channel();
        core.send(NodeRequest::GetMiningJob { reply }).await.unwrap();
        rx.await.unwrap()
    }

    async fn submit_block(core: &mpsc::Sender<NodeRequest>, block: Block) -> BlockOutcome {
        let (reply, rx) = oneshot::channel();
        core.send(NodeRequest::SubmitBlock {
            block: Box::new(block),
            reply,
        })
        .await
        .unwrap();
        rx.await.unwrap()
    }

    // Reproduce the live miner path: mine block 1 (coinbase), submit a tx, then
    // mine block 2 carrying that tx through the node core.
    #[tokio::test]
    async fn mines_past_block_1_with_a_mempool_transaction() {
        use ed25519_dalek::{Signer, SigningKey};
        use rand::rngs::OsRng;

        let (core, rx) = mpsc::channel(64);
        tokio::spawn(run_node(rx, None, REGTEST));

        let key = SigningKey::generate(&mut OsRng);
        let miner = key.verifying_key().to_bytes();

        // Block 1: coinbase to the miner.
        let j1 = get_job(&core).await;
        let (b1, _) = build_and_mine(
            &j1.parent,
            &j1.parent_state,
            REGTEST.initial_reward,
            miner,
            j1.normal,
            REGTEST.pow_limit,
            1,
        )
        .unwrap();
        assert!(matches!(
            submit_block(&core, b1).await,
            BlockOutcome::Accepted { is_new: true, .. }
        ));
        assert_eq!(height(&core).await, 1);

        // A signed transfer from the miner, nonce 0.
        let mut tx = Transaction {
            tx_type: crate::tx_type::NORMAL,
            sender: miner,
            recipient: [2u8; 32],
            amount: 10,
            fee: 0,
            nonce: 0,
            signature: [0u8; 64],
        };
        tx.signature = key.sign(&tx.signing_bytes()).to_bytes();
        let (reply, srx) = oneshot::channel();
        core.send(NodeRequest::SubmitTransaction { tx, reply })
            .await
            .unwrap();
        assert!(matches!(srx.await.unwrap(), SubmitResult::Accepted { is_new: true, .. }));

        // Block 2: the job should now carry the mempool tx.
        let j2 = get_job(&core).await;
        assert!(j2.normal.is_some(), "mining job did not include the mempool tx");
        let (b2, _) = build_and_mine(
            &j2.parent,
            &j2.parent_state,
            REGTEST.initial_reward,
            miner,
            j2.normal,
            REGTEST.pow_limit,
            2,
        )
        .unwrap();
        match submit_block(&core, b2).await {
            BlockOutcome::Accepted { is_new: true, height, .. } => assert_eq!(height, 2),
            BlockOutcome::Rejected { reason } => panic!("block 2 rejected: {reason}"),
            BlockOutcome::Accepted { is_new: false, .. } => panic!("block 2 already known"),
            BlockOutcome::Orphan => panic!("block 2 orphan"),
        }
        assert_eq!(height(&core).await, 2);
    }

    async fn submit_tx(core: &mpsc::Sender<NodeRequest>, tx: Transaction) -> SubmitResult {
        let (reply, rx) = oneshot::channel();
        core.send(NodeRequest::SubmitTransaction { tx, reply })
            .await
            .unwrap();
        rx.await.unwrap()
    }

    // S2 and S4 through the real submit path: a second transaction at the same
    // sender and nonce is rejected (not relayed), and so is an unknown type.
    #[tokio::test]
    async fn conflicting_and_unknown_type_transactions_are_rejected() {
        use ed25519_dalek::{Signer, SigningKey};
        use rand::rngs::OsRng;

        let (core, rx) = mpsc::channel(64);
        tokio::spawn(run_node(rx, None, REGTEST));
        let key = SigningKey::generate(&mut OsRng);
        let sender = key.verifying_key().to_bytes();

        // Fund the sender with block 1.
        let j1 = get_job(&core).await;
        let (b1, _) =
            build_and_mine(&j1.parent, &j1.parent_state, REGTEST.initial_reward, sender, None, REGTEST.pow_limit, 1)
                .unwrap();
        assert!(matches!(submit_block(&core, b1).await, BlockOutcome::Accepted { .. }));

        let signed = |tx_type: u8, amount: u64| {
            let mut tx = Transaction {
                tx_type,
                sender,
                recipient: [2u8; 32],
                amount,
                fee: 0,
                nonce: 0,
                signature: [0u8; 64],
            };
            tx.signature = key.sign(&tx.signing_bytes()).to_bytes();
            tx
        };

        assert!(matches!(
            submit_tx(&core, signed(crate::tx_type::NORMAL, 10)).await,
            SubmitResult::Accepted { is_new: true, .. }
        ));
        // Same transaction again: accepted but not new (not relayed again).
        assert!(matches!(
            submit_tx(&core, signed(crate::tx_type::NORMAL, 10)).await,
            SubmitResult::Accepted { is_new: false, .. }
        ));
        // A different amount at the same nonce is a conflict.
        match submit_tx(&core, signed(crate::tx_type::NORMAL, 11)).await {
            SubmitResult::Rejected { reason } => assert!(reason.contains("conflicts"), "{reason}"),
            SubmitResult::Accepted { .. } => panic!("conflicting transaction was accepted"),
        }
        // An unknown type code is rejected, not treated as a transfer.
        match submit_tx(&core, signed(9, 10)).await {
            SubmitResult::Rejected { reason } => assert!(reason.contains("unknown"), "{reason}"),
            SubmitResult::Accepted { .. } => panic!("unknown transaction type was accepted"),
        }
    }

    #[tokio::test]
    async fn chain_persists_across_restart() {
        let dir = temp_dir("persist");

        // First run: submit a mined block, then shut the node down.
        let (tx1, rx1) = mpsc::channel(16);
        let run1 = tokio::spawn(run_node(rx1, Some(dir.clone()), REGTEST));
        let genesis = Block::genesis(&REGTEST);
        let (block, _) = build_and_mine(
            &genesis.header,
            &State::new(),
            REGTEST.initial_reward,
            [1u8; 32],
            None,
            REGTEST.pow_limit,
            1,
        )
        .unwrap();
        let (reply, rx) = oneshot::channel();
        tx1.send(NodeRequest::SubmitBlock {
            block: Box::new(block),
            reply,
        })
        .await
        .unwrap();
        assert!(matches!(
            rx.await.unwrap(),
            BlockOutcome::Accepted { is_new: true, .. }
        ));
        assert_eq!(height(&tx1).await, 1);
        drop(tx1);
        run1.await.unwrap();

        // Second run on the same directory: the block is replayed from disk.
        let (tx2, rx2) = mpsc::channel(16);
        let run2 = tokio::spawn(run_node(rx2, Some(dir.clone()), REGTEST));
        assert_eq!(height(&tx2).await, 1);
        drop(tx2);
        run2.await.unwrap();

        std::fs::remove_dir_all(&dir).ok();
    }
}
