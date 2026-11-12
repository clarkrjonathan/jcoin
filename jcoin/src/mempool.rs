//! The mempool (pending transactions) and a bounded cache of recently seen ids.
//!
//! The two are separate on purpose. The mempool alone decides whether a
//! transaction is pending: a transaction that left the mempool (for example,
//! dropped by a reorg) is admitted again when it is resubmitted and still valid.
//! The seen cache only answers "has this node seen this id recently" for status
//! queries; it never blocks admission.
//!
//! A sender may have a chain of pending transactions with consecutive nonces:
//! the confirmed nonce `n`, then `n + 1`, and so on. Admission enforces three
//! rules against the tip state, so the chain is always minable in order:
//! - the nonce is exactly the next one after the sender's pending chain (no
//!   gaps, no reuse; reusing a pending nonce is a conflict, ADR 000),
//! - the confirmed balance covers the whole chain (`amount + fee` of every
//!   pending transaction, this one included; incoming pending credits are not
//!   counted),
//! - the chain is at most `MAX_PENDING_PER_SENDER` long.
//!
//! A transaction that skips ahead of the sender's chain (it arrived before an
//! earlier one) is held, not admitted: up to `MAX_HELD_PER_SENDER` per sender,
//! each for `HOLD_SECS`. When the gap fills, held transactions are admitted in
//! nonce order through the same rules. Held ones are not pending: they are not
//! mined, relayed, or counted.
//!
//! Both structures are owned by the node core task, so neither needs a lock.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::state::State;
use crate::Transaction;

/// Most transactions held in the mempool. Admission fails when it is full.
pub const MAX_MEMPOOL: usize = 10_000;

/// Most pending transactions one sender may have queued.
pub const MAX_PENDING_PER_SENDER: usize = 25;

/// Most transaction ids remembered by the seen cache (oldest evicted first).
pub const SEEN_CAPACITY: usize = 50_000;

/// Most out-of-order transactions held for one sender.
pub const MAX_HELD_PER_SENDER: usize = 16;

/// Most out-of-order transactions held in all.
pub const MAX_HELD: usize = 1_000;

/// How long an out-of-order transaction is held, in seconds.
pub const HOLD_SECS: u64 = 600;

/// A 32-byte transaction id.
pub type TxId = [u8; 32];

/// A successful admission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admitted {
    /// Newly added. The caller should relay it.
    New,
    /// The same transaction was already pending. Do not relay it again.
    AlreadyPending,
    /// It skips ahead of the sender's chain and is held until the gap fills.
    /// Do not relay it yet.
    Held,
}

/// Why a transaction was not admitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdmitError {
    /// A different transaction from the same sender with the same nonce is
    /// already pending. ADR 000: do not take part in propagating a conflict.
    Conflict { pending: TxId },
    /// The nonce is below the sender's confirmed nonce (already used).
    StaleNonce { expected: u64, got: u64 },
    /// The nonce skips ahead of the sender's pending chain.
    NonceGap { expected: u64, got: u64 },
    /// The confirmed balance does not cover the pending chain plus this one.
    InsufficientBalance { needed: u64, available: u64 },
    /// The sender already has `MAX_PENDING_PER_SENDER` pending transactions.
    SenderLimit,
    /// `amount + fee` overflows.
    Overflow,
    /// The mempool is at `MAX_MEMPOOL`.
    Full,
}

impl fmt::Display for AdmitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AdmitError::Conflict { pending } => write!(
                f,
                "conflicts with pending transaction {} (same sender and nonce)",
                hex::encode(pending)
            ),
            AdmitError::StaleNonce { expected, got } => {
                write!(f, "bad nonce: {got} is already used (next is {expected})")
            }
            AdmitError::NonceGap { expected, got } => {
                write!(f, "bad nonce: expected {expected}, got {got} (gap)")
            }
            AdmitError::InsufficientBalance { needed, available } => write!(
                f,
                "insufficient balance: pending transactions plus this one need {needed}, have {available}"
            ),
            AdmitError::SenderLimit => write!(
                f,
                "sender already has {MAX_PENDING_PER_SENDER} pending transactions"
            ),
            AdmitError::Overflow => write!(f, "amount + fee overflows"),
            AdmitError::Full => write!(f, "mempool is full"),
        }
    }
}

impl std::error::Error for AdmitError {}

/// A sender's view of the mempool, for wallets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct PendingSummary {
    /// The nonce the sender's next transaction must use.
    pub next_nonce: u64,
    /// Total `amount + fee` of the sender's pending transactions.
    pub pending_debit: u64,
    /// Number of the sender's pending transactions.
    pub count: usize,
}

/// Pending transactions, indexed by id and by sender, then nonce.
pub struct Mempool {
    by_id: HashMap<TxId, Transaction>,
    by_sender: HashMap<[u8; 32], BTreeMap<u64, TxId>>,
    capacity: usize,
    /// Out-of-order transactions by sender, then nonce, with the time each
    /// was held (seconds since the Unix epoch).
    held: HashMap<[u8; 32], BTreeMap<u64, (Transaction, u64)>>,
}

fn debit(tx: &Transaction) -> Option<u64> {
    tx.amount.checked_add(tx.fee)
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl Mempool {
    pub fn new(capacity: usize) -> Mempool {
        Mempool {
            by_id: HashMap::new(),
            by_sender: HashMap::new(),
            capacity,
            held: HashMap::new(),
        }
    }

    /// Number of out-of-order transactions held.
    pub fn held_len(&self) -> usize {
        self.held.values().map(|m| m.len()).sum()
    }

    pub fn len(&self) -> usize {
        self.by_id.len()
    }

    pub fn is_empty(&self) -> bool {
        self.by_id.is_empty()
    }

    pub fn contains(&self, id: &TxId) -> bool {
        self.by_id.contains_key(id)
    }

    /// The sender's pending chain on top of `state`: next nonce and total
    /// committed debit.
    pub fn summary(&self, sender: &[u8; 32], state: &State) -> PendingSummary {
        let confirmed = state.nonce(sender);
        let Some(chain) = self.by_sender.get(sender) else {
            return PendingSummary {
                next_nonce: confirmed,
                pending_debit: 0,
                count: 0,
            };
        };
        let pending_debit = chain
            .values()
            .filter_map(|id| self.by_id.get(id).and_then(debit))
            .fold(0u64, |acc, d| acc.saturating_add(d));
        PendingSummary {
            next_nonce: confirmed + chain.len() as u64,
            pending_debit,
            count: chain.len(),
        }
    }

    /// Admit a normal transaction whose signature and type the caller has
    /// already checked. Nonce and balance are checked here against `state`
    /// (the tip) and the sender's pending chain; see the module docs. One that
    /// skips ahead is held (`Admitted::Held`) if there is room.
    pub fn insert(&mut self, tx: Transaction, state: &State) -> Result<Admitted, AdmitError> {
        self.insert_at(tx, state, now_secs())
    }

    /// `insert` at time `now` (seconds since the Unix epoch).
    pub fn insert_at(&mut self, tx: Transaction, state: &State, now: u64) -> Result<Admitted, AdmitError> {
        let sender = tx.sender;
        match self.admit(tx.clone(), state) {
            Ok(Admitted::New) => {
                self.release_held(&sender, state, now);
                Ok(Admitted::New)
            }
            Err(AdmitError::NonceGap { expected, got })
                if got < expected + MAX_PENDING_PER_SENDER as u64 =>
            {
                let total = self.held_len();
                let held = self.held.entry(sender).or_default();
                if held.contains_key(&got) {
                    return Ok(Admitted::Held);
                }
                if held.len() >= MAX_HELD_PER_SENDER || total >= MAX_HELD {
                    if held.is_empty() {
                        self.held.remove(&sender);
                    }
                    return Err(AdmitError::NonceGap { expected, got });
                }
                held.insert(got, (tx, now));
                Ok(Admitted::Held)
            }
            other => other,
        }
    }

    /// Admit the sender's held transactions that now follow its chain, in
    /// nonce order, and drop those that expired or fell below it.
    fn release_held(&mut self, sender: &[u8; 32], state: &State, now: u64) {
        let Some(mut held) = self.held.remove(sender) else {
            return;
        };
        held.retain(|_, (_, at)| now.saturating_sub(*at) < HOLD_SECS);
        loop {
            let next = self.summary(sender, state).next_nonce;
            held.retain(|&nonce, _| nonce >= next);
            let Some((tx, _)) = held.remove(&next) else {
                break;
            };
            // One that fails admission (for example, unaffordable) is dropped;
            // the loop then finds the gap and stops.
            let _ = self.admit(tx, state);
        }
        if !held.is_empty() {
            self.held.insert(*sender, held);
        }
    }

    /// The admission rules for one transaction, without holding.
    fn admit(&mut self, tx: Transaction, state: &State) -> Result<Admitted, AdmitError> {
        let id = tx.tx_id();
        if self.by_id.contains_key(&id) {
            return Ok(Admitted::AlreadyPending);
        }
        let confirmed = state.nonce(&tx.sender);
        if tx.nonce < confirmed {
            return Err(AdmitError::StaleNonce {
                expected: confirmed,
                got: tx.nonce,
            });
        }
        if let Some(pending) = self.by_sender.get(&tx.sender).and_then(|c| c.get(&tx.nonce)) {
            return Err(AdmitError::Conflict { pending: *pending });
        }
        let summary = self.summary(&tx.sender, state);
        if tx.nonce != summary.next_nonce {
            return Err(AdmitError::NonceGap {
                expected: summary.next_nonce,
                got: tx.nonce,
            });
        }
        if summary.count >= MAX_PENDING_PER_SENDER {
            return Err(AdmitError::SenderLimit);
        }
        let needed = debit(&tx)
            .and_then(|d| d.checked_add(summary.pending_debit))
            .ok_or(AdmitError::Overflow)?;
        let available = state.balance(&tx.sender);
        if available < needed {
            return Err(AdmitError::InsufficientBalance { needed, available });
        }
        if self.by_id.len() >= self.capacity {
            return Err(AdmitError::Full);
        }
        self.by_sender
            .entry(tx.sender)
            .or_default()
            .insert(tx.nonce, id);
        self.by_id.insert(id, tx);
        Ok(Admitted::New)
    }

    /// Every pending transaction, each sender's in nonce order, so a node that
    /// receives them in this order can admit each one (a transaction is only
    /// admitted after the sender's earlier ones). Senders are in address
    /// order, so the result is deterministic.
    pub fn pending(&self) -> Vec<Transaction> {
        let mut senders: Vec<_> = self.by_sender.iter().collect();
        senders.sort_by_key(|(sender, _)| **sender);
        senders
            .into_iter()
            .flat_map(|(_, chain)| chain.values())
            .filter_map(|id| self.by_id.get(id).cloned())
            .collect()
    }

    /// The pending transaction to mine next on top of `state`: among those that
    /// apply now (the head of each sender's chain), the highest fee, with ties
    /// broken by the smallest id so the choice is deterministic.
    pub fn best_applicable(&self, state: &State) -> Option<Transaction> {
        self.by_id
            .iter()
            .filter(|(_, tx)| state.check(tx).is_ok())
            .max_by(|(a_id, a), (b_id, b)| a.fee.cmp(&b.fee).then(b_id.cmp(a_id)))
            .map(|(_, tx)| tx.clone())
    }

    /// Re-check every sender's pending chain against `state` (the new tip):
    /// drop transactions already included (nonce below the confirmed nonce),
    /// then keep the chain only while it stays consecutive and affordable.
    /// After a reorg this also drops the part of a chain that no longer fits.
    /// Then admits held transactions that now follow a chain.
    pub fn prune(&mut self, state: &State) {
        self.prune_at(state, now_secs());
    }

    /// `prune` at time `now` (seconds since the Unix epoch).
    pub fn prune_at(&mut self, state: &State, now: u64) {
        self.prune_pending(state);
        let senders: Vec<[u8; 32]> = self.held.keys().copied().collect();
        for sender in senders {
            self.release_held(&sender, state, now);
        }
    }

    fn prune_pending(&mut self, state: &State) {
        let mut drop_ids = Vec::new();
        // Ids that leave without being confirmed (a broken or unaffordable
        // chain), as opposed to those included in a block.
        let mut dropped_unconfirmed = Vec::new();
        for (sender, chain) in self.by_sender.iter_mut() {
            let mut expected = state.nonce(sender);
            let available = state.balance(sender);
            let mut committed: u64 = 0;
            let mut broken = false;
            chain.retain(|&nonce, id| {
                let keep = !broken
                    && nonce == expected
                    && self
                        .by_id
                        .get(id)
                        .and_then(debit)
                        .and_then(|d| committed.checked_add(d))
                        .is_some_and(|total| {
                            if total <= available {
                                committed = total;
                                true
                            } else {
                                false
                            }
                        });
                if keep {
                    expected += 1;
                } else {
                    // Below the confirmed nonce: included. Otherwise the chain
                    // is broken here, and everything after it goes too.
                    if nonce >= expected {
                        broken = true;
                        dropped_unconfirmed.push((*id, nonce));
                    }
                    drop_ids.push(*id);
                }
                keep
            });
        }
        self.by_sender.retain(|_, chain| !chain.is_empty());
        for id in drop_ids {
            self.by_id.remove(&id);
        }
        for (id, nonce) in dropped_unconfirmed {
            crate::log_info!(
                [("event", "pending_dropped".to_string()), ("id", hex::encode(id)), ("nonce", nonce.to_string())];
                "[node] pending transfer {} dropped without confirming (nonce {nonce})",
                hex::encode(id)
            );
        }
    }
}

/// A bounded set of recently seen transaction ids. When full, the oldest id is
/// evicted.
pub struct SeenCache {
    set: HashSet<TxId>,
    order: VecDeque<TxId>,
    capacity: usize,
}

impl SeenCache {
    pub fn new(capacity: usize) -> SeenCache {
        SeenCache {
            set: HashSet::new(),
            order: VecDeque::new(),
            capacity,
        }
    }

    pub fn insert(&mut self, id: TxId) {
        if self.capacity == 0 || !self.set.insert(id) {
            return;
        }
        self.order.push_back(id);
        while self.order.len() > self.capacity {
            if let Some(old) = self.order.pop_front() {
                self.set.remove(&old);
            }
        }
    }

    pub fn contains(&self, id: &TxId) -> bool {
        self.set.contains(id)
    }

    pub fn len(&self) -> usize {
        self.set.len()
    }

    pub fn is_empty(&self) -> bool {
        self.set.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consensus::REGTEST;
    use crate::tx_type;

    const REWARD: u64 = REGTEST.initial_reward;

    fn tx(sender: u8, nonce: u64, amount: u64, fee: u64) -> Transaction {
        Transaction {
            tx_type: tx_type::NORMAL,
            sender: [sender; 32],
            recipient: [99u8; 32],
            amount,
            fee,
            nonce,
            signature: [0u8; 64],
        }
    }

    /// A state where senders 1, 2, and 3 each hold one block reward.
    fn funded() -> State {
        let mut s = State::new();
        for who in [1u8, 2, 3] {
            s.apply(&Transaction::coinbase([who; 32], REWARD)).unwrap();
        }
        s
    }

    #[test]
    fn pending_lists_each_senders_transactions_in_nonce_order() {
        let state = funded();
        let mut pool = Mempool::new(10);
        assert!(pool.pending().is_empty());
        for t in [tx(2, 0, 5, 0), tx(1, 0, 5, 0), tx(1, 1, 5, 0), tx(2, 1, 5, 0), tx(1, 2, 5, 0)] {
            assert_eq!(pool.insert(t, &state), Ok(Admitted::New));
        }
        let order: Vec<(u8, u64)> = pool.pending().iter().map(|t| (t.sender[0], t.nonce)).collect();
        assert_eq!(order, [(1, 0), (1, 1), (1, 2), (2, 0), (2, 1)]);

        // Replayed in that order into an empty pool, every one is admitted.
        let mut other = Mempool::new(10);
        for t in pool.pending() {
            assert_eq!(other.insert(t, &state), Ok(Admitted::New));
        }
        assert_eq!(other.len(), 5);
    }

    #[test]
    fn same_transaction_twice_is_already_pending() {
        let state = funded();
        let mut pool = Mempool::new(10);
        assert_eq!(pool.insert(tx(1, 0, 5, 0), &state), Ok(Admitted::New));
        assert_eq!(pool.insert(tx(1, 0, 5, 0), &state), Ok(Admitted::AlreadyPending));
        assert_eq!(pool.len(), 1);
    }

    // S4: one key signing many variants at the same nonce must not flood the
    // mempool. Only the first is admitted.
    #[test]
    fn conflicting_transactions_at_the_same_nonce_are_rejected() {
        let state = funded();
        let mut pool = Mempool::new(10);
        let first = tx(1, 0, 5, 0);
        let first_id = first.tx_id();
        assert_eq!(pool.insert(first, &state), Ok(Admitted::New));
        for amount in 6..20 {
            assert_eq!(
                pool.insert(tx(1, 0, amount, 0), &state),
                Err(AdmitError::Conflict { pending: first_id })
            );
        }
        assert_eq!(pool.len(), 1);
    }

    #[test]
    fn a_sender_can_queue_consecutive_nonces() {
        let state = funded();
        let mut pool = Mempool::new(10);
        for nonce in 0..4 {
            assert_eq!(pool.insert(tx(1, nonce, 5, 1), &state), Ok(Admitted::New));
        }
        let s = pool.summary(&[1u8; 32], &state);
        assert_eq!(s.next_nonce, 4);
        assert_eq!(s.pending_debit, 4 * 6);
        assert_eq!(s.count, 4);
        // Only the head of the chain applies on the tip.
        assert_eq!(pool.best_applicable(&state).unwrap().nonce, 0);
    }

    #[test]
    fn far_gaps_and_stale_nonces_are_rejected() {
        let mut state = funded();
        let mut pool = Mempool::new(10);
        let far = MAX_PENDING_PER_SENDER as u64;
        assert_eq!(
            pool.insert(tx(1, far, 5, 0), &state),
            Err(AdmitError::NonceGap { expected: 0, got: far })
        );
        // Confirm nonce 0, then nonce 0 again is stale.
        state.apply(&tx(1, 0, 5, 0)).unwrap();
        assert_eq!(
            pool.insert(tx(1, 0, 7, 0), &state),
            Err(AdmitError::StaleNonce { expected: 1, got: 0 })
        );
    }

    #[test]
    fn an_out_of_order_transfer_is_held_until_the_gap_fills() {
        let state = funded();
        let mut pool = Mempool::new(10);
        assert_eq!(pool.insert_at(tx(1, 2, 5, 0), &state, 100), Ok(Admitted::Held));
        assert_eq!(pool.insert_at(tx(1, 1, 5, 0), &state, 100), Ok(Admitted::Held));
        assert_eq!(pool.len(), 0);
        assert_eq!(pool.held_len(), 2);
        // Nonce 0 fills the gap; 1 and 2 follow.
        assert_eq!(pool.insert_at(tx(1, 0, 5, 0), &state, 101), Ok(Admitted::New));
        assert_eq!(pool.len(), 3);
        assert_eq!(pool.held_len(), 0);
        assert_eq!(pool.summary(&[1u8; 32], &state).next_nonce, 3);
    }

    #[test]
    fn a_block_confirming_the_gap_releases_held_transfers() {
        let mut state = funded();
        let mut pool = Mempool::new(10);
        assert_eq!(pool.insert_at(tx(1, 1, 5, 0), &state, 100), Ok(Admitted::Held));
        // Nonce 0 arrives in a block, not through the mempool.
        state.apply(&tx(1, 0, 5, 0)).unwrap();
        pool.prune_at(&state, 110);
        assert_eq!(pool.len(), 1);
        assert_eq!(pool.held_len(), 0);
    }

    #[test]
    fn held_transfers_expire_and_are_bounded() {
        let state = funded();
        let mut pool = Mempool::new(100);
        assert_eq!(pool.insert_at(tx(1, 1, 5, 0), &state, 100), Ok(Admitted::Held));
        // Past HOLD_SECS the held one is dropped when nonce 0 arrives.
        assert_eq!(pool.insert_at(tx(1, 0, 5, 0), &state, 100 + HOLD_SECS), Ok(Admitted::New));
        assert_eq!(pool.len(), 1);
        assert_eq!(pool.held_len(), 0);
        // At most MAX_HELD_PER_SENDER for one sender.
        for n in 0..MAX_HELD_PER_SENDER as u64 {
            assert_eq!(pool.insert_at(tx(2, n + 2, 1, 0), &state, 200), Ok(Admitted::Held));
        }
        assert!(matches!(
            pool.insert_at(tx(2, MAX_HELD_PER_SENDER as u64 + 2, 1, 0), &state, 200),
            Err(AdmitError::NonceGap { .. })
        ));
    }

    #[test]
    fn the_whole_chain_must_be_affordable() {
        let state = funded();
        let mut pool = Mempool::new(10);
        let half = REWARD / 2;
        assert_eq!(pool.insert(tx(1, 0, half, 0), &state), Ok(Admitted::New));
        assert_eq!(pool.insert(tx(1, 1, half, 0), &state), Ok(Admitted::New));
        // Each alone is affordable, but the chain now needs more than REWARD.
        assert_eq!(
            pool.insert(tx(1, 2, 1, 0), &state),
            Err(AdmitError::InsufficientBalance {
                needed: REWARD + 1,
                available: REWARD
            })
        );
    }

    #[test]
    fn per_sender_limit_is_enforced() {
        let state = funded();
        let mut pool = Mempool::new(100);
        for nonce in 0..MAX_PENDING_PER_SENDER as u64 {
            pool.insert(tx(1, nonce, 1, 0), &state).unwrap();
        }
        assert_eq!(
            pool.insert(tx(1, MAX_PENDING_PER_SENDER as u64, 1, 0), &state),
            Err(AdmitError::SenderLimit)
        );
    }

    #[test]
    fn capacity_is_enforced() {
        let state = funded();
        let mut pool = Mempool::new(2);
        assert_eq!(pool.insert(tx(1, 0, 1, 0), &state), Ok(Admitted::New));
        assert_eq!(pool.insert(tx(2, 0, 1, 0), &state), Ok(Admitted::New));
        assert_eq!(pool.insert(tx(3, 0, 1, 0), &state), Err(AdmitError::Full));
    }

    #[test]
    fn best_applicable_prefers_the_highest_fee() {
        let state = funded();
        let mut pool = Mempool::new(10);
        pool.insert(tx(1, 0, 5, 1), &state).unwrap();
        pool.insert(tx(2, 0, 5, 9), &state).unwrap();
        pool.insert(tx(3, 0, 5, 4), &state).unwrap();
        // Sender 2's second transaction has the top fee but does not apply yet.
        pool.insert(tx(2, 1, 5, 50), &state).unwrap();
        let best = pool.best_applicable(&state).unwrap();
        assert_eq!((best.sender, best.nonce), ([2u8; 32], 0));
    }

    // Mining the head moves the chain forward: the included transaction goes,
    // the next one becomes the head.
    #[test]
    fn prune_after_inclusion_keeps_the_rest_of_the_chain() {
        let mut state = funded();
        let mut pool = Mempool::new(10);
        let t0 = tx(1, 0, 5, 0);
        pool.insert(t0.clone(), &state).unwrap();
        pool.insert(tx(1, 1, 5, 0), &state).unwrap();
        state.apply(&t0).unwrap();
        pool.prune(&state);
        assert_eq!(pool.len(), 1);
        assert!(!pool.contains(&t0.tx_id()));
        assert_eq!(pool.best_applicable(&state).unwrap().nonce, 1);
        assert_eq!(pool.summary(&[1u8; 32], &state).next_nonce, 2);
    }

    // After a reorg to a state with less balance, the part of the chain that
    // no longer fits is dropped, from the first unaffordable transaction on.
    #[test]
    fn prune_drops_the_unaffordable_tail() {
        let state = funded();
        let mut pool = Mempool::new(10);
        let third = REWARD / 3;
        for nonce in 0..3 {
            pool.insert(tx(1, nonce, third, 0), &state).unwrap();
        }
        // New tip: sender 1 holds only enough for the first two.
        let mut poorer = State::new();
        poorer.apply(&Transaction::coinbase([1u8; 32], 2 * third + 1)).unwrap();
        pool.prune(&poorer);
        assert_eq!(pool.len(), 2);
        assert_eq!(pool.summary(&[1u8; 32], &poorer).next_nonce, 2);
    }

    // S3: a transaction that left the mempool is admitted again on resubmit.
    #[test]
    fn a_removed_transaction_can_be_readmitted() {
        let state = funded();
        let mut pool = Mempool::new(10);
        let t = tx(1, 0, 5, 0);
        pool.insert(t.clone(), &state).unwrap();
        // Simulate a reorg to a state where the sender has no funds: pruned.
        pool.prune(&State::new());
        assert!(!pool.contains(&t.tx_id()));
        // Back on a state where it applies, the same tx is new again.
        assert_eq!(pool.insert(t, &state), Ok(Admitted::New));
    }

    #[test]
    fn seen_cache_evicts_the_oldest() {
        let mut seen = SeenCache::new(2);
        seen.insert([1u8; 32]);
        seen.insert([2u8; 32]);
        seen.insert([2u8; 32]); // duplicate: no effect
        seen.insert([3u8; 32]);
        assert_eq!(seen.len(), 2);
        assert!(!seen.contains(&[1u8; 32]));
        assert!(seen.contains(&[2u8; 32]));
        assert!(seen.contains(&[3u8; 32]));
    }
}
