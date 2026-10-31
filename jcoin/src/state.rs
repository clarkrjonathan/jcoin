//! The account state: per-key balance and nonce, with a canonical `state_root`.
//!
//! The account model (not UTXO) is JeffyCoin's v1 choice. Accounts are kept in a
//! `BTreeMap` so serialization is deterministic (keys in ascending order), which
//! is what makes `state_root` a stable commitment across nodes.

use std::collections::BTreeMap;

use sha2::{Digest, Sha256};

use crate::Transaction;

/// One account's consensus state.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Account {
    pub balance: u64,
    /// Next expected transaction nonce for this account.
    pub nonce: u64,
}

/// Why applying a transaction to the state failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StateError {
    /// The transaction nonce did not match the account's expected nonce.
    BadNonce { expected: u64, got: u64 },
    /// The sender cannot cover `amount + fee`.
    InsufficientBalance { needed: u64, available: u64 },
    /// A balance would overflow `u64`.
    Overflow,
}

impl std::fmt::Display for StateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StateError::BadNonce { expected, got } => {
                write!(f, "bad nonce: expected {expected}, got {got}")
            }
            StateError::InsufficientBalance { needed, available } => {
                write!(f, "insufficient balance: need {needed}, have {available}")
            }
            StateError::Overflow => write!(f, "balance overflow"),
        }
    }
}

impl std::error::Error for StateError {}

/// The full account table.
#[derive(Debug, Clone, Default)]
pub struct State {
    accounts: BTreeMap<[u8; 32], Account>,
}

impl State {
    pub fn new() -> Self {
        State::default()
    }

    pub fn balance(&self, address: &[u8; 32]) -> u64 {
        self.accounts.get(address).map(|a| a.balance).unwrap_or(0)
    }

    pub fn nonce(&self, address: &[u8; 32]) -> u64 {
        self.accounts.get(address).map(|a| a.nonce).unwrap_or(0)
    }

    /// Credit an account, creating it if needed. Used for coinbase output.
    fn credit(&mut self, address: &[u8; 32], amount: u64) -> Result<(), StateError> {
        let account = self.accounts.entry(*address).or_default();
        account.balance = account.balance.checked_add(amount).ok_or(StateError::Overflow)?;
        Ok(())
    }

    /// Apply one transaction to the state.
    ///
    /// A coinbase credits its recipient with `amount` (the block-level rules
    /// bound that amount to `reward + fees`). A normal transfer checks the nonce
    /// and that the sender covers `amount + fee`, debits the sender by
    /// `amount + fee`, credits the recipient by `amount`, and bumps the nonce.
    /// The fee is not credited here; it is minted to the miner via the coinbase.
    pub fn apply(&mut self, tx: &Transaction) -> Result<(), StateError> {
        if tx.is_coinbase() {
            return self.credit(&tx.recipient, tx.amount);
        }

        let sender = self.accounts.entry(tx.sender).or_default();
        if tx.nonce != sender.nonce {
            return Err(StateError::BadNonce {
                expected: sender.nonce,
                got: tx.nonce,
            });
        }
        let debit = tx.amount.checked_add(tx.fee).ok_or(StateError::Overflow)?;
        if sender.balance < debit {
            return Err(StateError::InsufficientBalance {
                needed: debit,
                available: sender.balance,
            });
        }
        sender.balance -= debit;
        sender.nonce += 1;

        self.credit(&tx.recipient, tx.amount)
    }

    /// Canonical serialization: for each account in key order,
    /// `address (32) | balance (8) | nonce (8)`, all big-endian.
    pub fn serialize(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(self.accounts.len() * 48);
        for (address, account) in &self.accounts {
            bytes.extend_from_slice(address);
            bytes.extend_from_slice(&account.balance.to_be_bytes());
            bytes.extend_from_slice(&account.nonce.to_be_bytes());
        }
        bytes
    }

    /// SHA-256 over the canonical serialization. This is the block `state_root`
    /// for v1 (a Merkle root may replace it later).
    pub fn state_root(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(self.serialize());
        hasher.finalize().into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tx_type;

    fn normal(sender: [u8; 32], recipient: [u8; 32], amount: u64, fee: u64, nonce: u64) -> Transaction {
        Transaction {
            tx_type: tx_type::NORMAL,
            sender,
            recipient,
            amount,
            fee,
            nonce,
            signature: [0u8; 64],
        }
    }

    #[test]
    fn coinbase_credits_recipient() {
        let mut state = State::new();
        let miner = [1u8; 32];
        state.apply(&Transaction::coinbase(miner, 50)).unwrap();
        assert_eq!(state.balance(&miner), 50);
    }

    #[test]
    fn normal_transfer_moves_funds_and_bumps_nonce() {
        let mut state = State::new();
        let a = [1u8; 32];
        let b = [2u8; 32];
        state.apply(&Transaction::coinbase(a, 100)).unwrap();

        state.apply(&normal(a, b, 30, 5, 0)).unwrap();
        assert_eq!(state.balance(&a), 65); // 100 - (30 + 5)
        assert_eq!(state.balance(&b), 30);
        assert_eq!(state.nonce(&a), 1);
    }

    #[test]
    fn wrong_nonce_is_rejected() {
        let mut state = State::new();
        let a = [1u8; 32];
        state.apply(&Transaction::coinbase(a, 100)).unwrap();
        let err = state.apply(&normal(a, [2u8; 32], 10, 0, 5)).unwrap_err();
        assert_eq!(err, StateError::BadNonce { expected: 0, got: 5 });
    }

    #[test]
    fn overspend_is_rejected() {
        let mut state = State::new();
        let a = [1u8; 32];
        state.apply(&Transaction::coinbase(a, 10)).unwrap();
        let err = state.apply(&normal(a, [2u8; 32], 10, 5, 0)).unwrap_err();
        assert_eq!(err, StateError::InsufficientBalance { needed: 15, available: 10 });
    }

    #[test]
    fn state_root_changes_with_state_and_is_deterministic() {
        let mut s1 = State::new();
        let mut s2 = State::new();
        let empty = s1.state_root();
        s1.apply(&Transaction::coinbase([9u8; 32], 7)).unwrap();
        s2.apply(&Transaction::coinbase([9u8; 32], 7)).unwrap();
        assert_ne!(s1.state_root(), empty);
        assert_eq!(s1.state_root(), s2.state_root());
    }
}
