//! The mempool (pending transactions) and a bounded cache of recently seen ids.
//!
//! The two are separate on purpose. The mempool alone decides whether a
//! transaction is pending: a transaction that left the mempool (for example,
//! dropped by a reorg) is admitted again when it is resubmitted and still valid.
//! The seen cache only answers "has this node seen this id recently" for status
//! queries; it never blocks admission.
//!
//! Both are owned by the node core task, so neither needs a lock.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt;

use crate::state::State;
use crate::Transaction;

/// Most transactions held in the mempool. Admission fails when it is full.
pub const MAX_MEMPOOL: usize = 10_000;

/// Most transaction ids remembered by the seen cache (oldest evicted first).
pub const SEEN_CAPACITY: usize = 50_000;

/// A 32-byte transaction id.
pub type TxId = [u8; 32];

/// A successful admission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admitted {
    /// Newly added. The caller should relay it.
    New,
    /// The same transaction was already pending. Do not relay it again.
    AlreadyPending,
}

/// Why a transaction was not admitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdmitError {
    /// A different transaction from the same sender with the same nonce is
    /// already pending. ADR 000: do not take part in propagating a conflict.
    Conflict { pending: TxId },
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
            AdmitError::Full => write!(f, "mempool is full"),
        }
    }
}

impl std::error::Error for AdmitError {}

/// Pending transactions, indexed by id and by `(sender, nonce)`.
pub struct Mempool {
    by_id: HashMap<TxId, Transaction>,
    by_slot: HashMap<([u8; 32], u64), TxId>,
    capacity: usize,
}

impl Mempool {
    pub fn new(capacity: usize) -> Mempool {
        Mempool {
            by_id: HashMap::new(),
            by_slot: HashMap::new(),
            capacity,
        }
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

    /// Admit a transaction that the caller has already validated against the
    /// tip state. Enforces one pending transaction per `(sender, nonce)` and
    /// the size cap.
    pub fn insert(&mut self, tx: Transaction) -> Result<Admitted, AdmitError> {
        let id = tx.tx_id();
        if self.by_id.contains_key(&id) {
            return Ok(Admitted::AlreadyPending);
        }
        let slot = (tx.sender, tx.nonce);
        if let Some(pending) = self.by_slot.get(&slot) {
            return Err(AdmitError::Conflict { pending: *pending });
        }
        if self.by_id.len() >= self.capacity {
            return Err(AdmitError::Full);
        }
        self.by_slot.insert(slot, id);
        self.by_id.insert(id, tx);
        Ok(Admitted::New)
    }

    /// The pending transaction to mine next on top of `state`: among those that
    /// apply, the highest fee, with ties broken by the smallest id so the choice
    /// is deterministic.
    pub fn best_applicable(&self, state: &State) -> Option<Transaction> {
        self.by_id
            .iter()
            .filter(|(_, tx)| state.check(tx).is_ok())
            .max_by(|(a_id, a), (b_id, b)| a.fee.cmp(&b.fee).then(b_id.cmp(a_id)))
            .map(|(_, tx)| tx.clone())
    }

    /// Drop every pending transaction that no longer applies on `state`
    /// (already included in a block, a stale nonce, or a balance that no longer
    /// covers it).
    pub fn prune(&mut self, state: &State) {
        let by_slot = &mut self.by_slot;
        self.by_id.retain(|_, tx| {
            let keep = state.check(tx).is_ok();
            if !keep {
                by_slot.remove(&(tx.sender, tx.nonce));
            }
            keep
        });
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
            s.apply(&Transaction::coinbase([who; 32], REGTEST.initial_reward)).unwrap();
        }
        s
    }

    #[test]
    fn same_transaction_twice_is_already_pending() {
        let mut pool = Mempool::new(10);
        assert_eq!(pool.insert(tx(1, 0, 5, 0)), Ok(Admitted::New));
        assert_eq!(pool.insert(tx(1, 0, 5, 0)), Ok(Admitted::AlreadyPending));
        assert_eq!(pool.len(), 1);
    }

    // S4: one key signing many variants at the same nonce must not flood the
    // mempool. Only the first is admitted.
    #[test]
    fn conflicting_transactions_at_the_same_nonce_are_rejected() {
        let mut pool = Mempool::new(10);
        let first = tx(1, 0, 5, 0);
        let first_id = first.tx_id();
        assert_eq!(pool.insert(first), Ok(Admitted::New));
        for amount in 6..20 {
            assert_eq!(
                pool.insert(tx(1, 0, amount, 0)),
                Err(AdmitError::Conflict { pending: first_id })
            );
        }
        assert_eq!(pool.len(), 1);
    }

    #[test]
    fn capacity_is_enforced() {
        let mut pool = Mempool::new(2);
        assert_eq!(pool.insert(tx(1, 0, 1, 0)), Ok(Admitted::New));
        assert_eq!(pool.insert(tx(2, 0, 1, 0)), Ok(Admitted::New));
        assert_eq!(pool.insert(tx(3, 0, 1, 0)), Err(AdmitError::Full));
    }

    #[test]
    fn best_applicable_prefers_the_highest_fee() {
        let state = funded();
        let mut pool = Mempool::new(10);
        pool.insert(tx(1, 0, 5, 1)).unwrap();
        pool.insert(tx(2, 0, 5, 9)).unwrap();
        pool.insert(tx(3, 0, 5, 4)).unwrap();
        // Not applicable: wrong nonce for sender 4 (who has nothing anyway).
        pool.insert(tx(4, 3, 5, 100)).unwrap();
        assert_eq!(pool.best_applicable(&state).unwrap().sender, [2u8; 32]);
    }

    #[test]
    fn prune_removes_both_indexes() {
        let mut state = funded();
        let mut pool = Mempool::new(10);
        let t = tx(1, 0, 5, 0);
        pool.insert(t.clone()).unwrap();
        // Include it: the sender's nonce moves to 1, so it no longer applies.
        state.apply(&t).unwrap();
        pool.prune(&state);
        assert!(pool.is_empty());
        // The (sender, nonce) slot is free again: a new tx at nonce 1 is new,
        // and nothing remains at nonce 0 to conflict with.
        assert_eq!(pool.insert(tx(1, 1, 5, 0)), Ok(Admitted::New));
    }

    // S3: a transaction that left the mempool is admitted again on resubmit.
    #[test]
    fn a_removed_transaction_can_be_readmitted() {
        let state = funded();
        let mut pool = Mempool::new(10);
        let t = tx(1, 0, 5, 0);
        pool.insert(t.clone()).unwrap();
        // Simulate a reorg to a state where the sender has no funds: pruned.
        pool.prune(&State::new());
        assert!(!pool.contains(&t.tx_id()));
        // Back on a state where it applies, the same tx is new again.
        assert!(state.check(&t).is_ok());
        assert_eq!(pool.insert(t), Ok(Admitted::New));
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
