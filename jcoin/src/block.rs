//! Blocks, the 156-byte header, proof of work, genesis, mining, and validation.
//!
//! A v1 block body is a coinbase first, then zero or one normal transaction. The
//! header commits to the transactions and to the resulting account state. Proof
//! of work is `SHA256(header) < target`, compared as big-endian 256-bit numbers.

use sha2::{Digest, Sha256};

use crate::state::{State, StateError};
use crate::Transaction;

/// Header size in bytes: 4 + 8 + 32 + 32 + 32 + 8 + 32 + 8.
pub const HEADER_LEN: usize = 156;

/// Current block/consensus version.
pub const BLOCK_VERSION: u32 = 1;

/// Flat block reward for v1 (no halving yet).
pub const BLOCK_REWARD: u64 = 50;

/// The 32-byte plaintext marker placed in the genesis `previous_block_hash`.
pub const GENESIS_MARKER: &[u8; 32] = b"I am 20 years old, I love jeffy!";

/// Fixed genesis timestamp.
pub const GENESIS_TIMESTAMP: u64 = 0;

/// Static proof-of-work target for v1 (no retargeting yet): the top 16 bits must
/// be zero, so a block takes on the order of 2^16 hashes to find. Kept easy so
/// local mining is fast; a real difficulty schedule is a later step.
pub const STATIC_TARGET: [u8; 32] = [
    0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
    0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
];

/// A block header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockHeader {
    pub version: u32,
    pub height: u64,
    pub previous_block_hash: [u8; 32],
    pub transactions_hash: [u8; 32],
    pub state_root: [u8; 32],
    pub timestamp: u64,
    pub target: [u8; 32],
    pub nonce: u64,
}

impl BlockHeader {
    /// Canonical serialization, all multi-byte fields big-endian, in field order.
    pub fn serialize(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(HEADER_LEN);
        bytes.extend_from_slice(&self.version.to_be_bytes());
        bytes.extend_from_slice(&self.height.to_be_bytes());
        bytes.extend_from_slice(&self.previous_block_hash);
        bytes.extend_from_slice(&self.transactions_hash);
        bytes.extend_from_slice(&self.state_root);
        bytes.extend_from_slice(&self.timestamp.to_be_bytes());
        bytes.extend_from_slice(&self.target);
        bytes.extend_from_slice(&self.nonce.to_be_bytes());
        bytes
    }

    /// Parse a header from its 156-byte serialization.
    pub fn from_bytes(b: &[u8]) -> Option<BlockHeader> {
        if b.len() != HEADER_LEN {
            return None;
        }
        Some(BlockHeader {
            version: u32::from_be_bytes(b[0..4].try_into().ok()?),
            height: u64::from_be_bytes(b[4..12].try_into().ok()?),
            previous_block_hash: b[12..44].try_into().ok()?,
            transactions_hash: b[44..76].try_into().ok()?,
            state_root: b[76..108].try_into().ok()?,
            timestamp: u64::from_be_bytes(b[108..116].try_into().ok()?),
            target: b[116..148].try_into().ok()?,
            nonce: u64::from_be_bytes(b[148..156].try_into().ok()?),
        })
    }

    /// The block hash: SHA-256 over the serialized header.
    pub fn block_hash(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(self.serialize());
        hasher.finalize().into()
    }

    /// True if the block hash is strictly below the target. Both are big-endian,
    /// so the byte-array comparison is the numeric comparison.
    pub fn meets_target(&self) -> bool {
        self.block_hash() < self.target
    }
}

/// Hash committing to a transaction list: SHA-256 over each transaction's
/// canonical serialization in order.
pub fn transactions_hash(transactions: &[Transaction]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    for tx in transactions {
        hasher.update(tx.serialize_to_bytes());
    }
    hasher.finalize().into()
}

/// A full block: header plus body (coinbase first, then zero or one normal tx).
#[derive(Debug, Clone)]
pub struct Block {
    pub header: BlockHeader,
    pub transactions: Vec<Transaction>,
}

/// Why a block is invalid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BlockError {
    BadHeight,
    BadParentHash,
    WrongTarget,
    PowNotMet,
    BadBody,
    BadTransactionsHash,
    BadStateRoot,
    CoinbaseTooLarge,
    BadSignature,
    Apply(StateError),
}

impl std::fmt::Display for BlockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BlockError::BadHeight => write!(f, "height is not parent height + 1"),
            BlockError::BadParentHash => write!(f, "previous_block_hash does not match parent"),
            BlockError::WrongTarget => write!(f, "target does not match consensus"),
            BlockError::PowNotMet => write!(f, "block hash does not meet the target"),
            BlockError::BadBody => write!(f, "body is not a coinbase plus at most one normal tx"),
            BlockError::BadTransactionsHash => write!(f, "transactions_hash mismatch"),
            BlockError::BadStateRoot => write!(f, "state_root mismatch"),
            BlockError::CoinbaseTooLarge => write!(f, "coinbase exceeds reward plus fees"),
            BlockError::BadSignature => write!(f, "a normal transaction has an invalid signature"),
            BlockError::Apply(e) => write!(f, "state apply failed: {e}"),
        }
    }
}

impl std::error::Error for BlockError {}

impl Block {
    /// The fixed genesis block. All nodes trust the same genesis; it is not
    /// proof-of-work validated. Height 0, empty body, empty state.
    pub fn genesis() -> Block {
        let transactions: Vec<Transaction> = Vec::new();
        let header = BlockHeader {
            version: BLOCK_VERSION,
            height: 0,
            previous_block_hash: *GENESIS_MARKER,
            transactions_hash: transactions_hash(&transactions),
            state_root: State::new().state_root(),
            timestamp: GENESIS_TIMESTAMP,
            target: STATIC_TARGET,
            nonce: 0,
        };
        Block {
            header,
            transactions,
        }
    }

    pub fn hash(&self) -> [u8; 32] {
        self.header.block_hash()
    }

    /// Split the body into the coinbase and the normal transactions, enforcing
    /// the v1 shape: first is a coinbase, the rest are normal, at most one extra.
    fn split_body(&self) -> Result<(&Transaction, &[Transaction]), BlockError> {
        let first = self.transactions.first().ok_or(BlockError::BadBody)?;
        if !first.is_coinbase() {
            return Err(BlockError::BadBody);
        }
        let rest = &self.transactions[1..];
        if rest.len() > 1 || rest.iter().any(|t| t.is_coinbase()) {
            return Err(BlockError::BadBody);
        }
        Ok((first, rest))
    }

    /// Validate this block against its parent header and the parent's state, and
    /// return the resulting state. The normal transactions are applied first,
    /// then the coinbase, so a miner cannot spend the reward within the same
    /// block.
    pub fn validate_and_apply(
        &self,
        parent: &BlockHeader,
        parent_state: &State,
        reward: u64,
        expected_target: &[u8; 32],
    ) -> Result<State, BlockError> {
        if self.header.height != parent.height + 1 {
            return Err(BlockError::BadHeight);
        }
        if self.header.previous_block_hash != parent.block_hash() {
            return Err(BlockError::BadParentHash);
        }
        if &self.header.target != expected_target {
            return Err(BlockError::WrongTarget);
        }
        if !self.header.meets_target() {
            return Err(BlockError::PowNotMet);
        }

        let (coinbase, normals) = self.split_body()?;

        if self.header.transactions_hash != transactions_hash(&self.transactions) {
            return Err(BlockError::BadTransactionsHash);
        }

        let mut fees: u64 = 0;
        for tx in normals {
            fees = fees.checked_add(tx.fee).ok_or(BlockError::CoinbaseTooLarge)?;
            if !tx.verify_signature() {
                return Err(BlockError::BadSignature);
            }
        }
        let max_coinbase = reward.checked_add(fees).ok_or(BlockError::CoinbaseTooLarge)?;
        if coinbase.amount > max_coinbase {
            return Err(BlockError::CoinbaseTooLarge);
        }

        let mut state = parent_state.clone();
        for tx in normals {
            state.apply(tx).map_err(BlockError::Apply)?;
        }
        state.apply(coinbase).map_err(BlockError::Apply)?;

        if self.header.state_root != state.state_root() {
            return Err(BlockError::BadStateRoot);
        }

        Ok(state)
    }
}

/// Assemble and mine the next block on top of `parent`.
///
/// Builds the coinbase (reward plus the normal transaction's fee) to `miner`,
/// applies the body to the parent state to get the state root, then searches
/// nonces until the header meets `target`. Returns the mined block and its
/// resulting state.
pub fn build_and_mine(
    parent: &BlockHeader,
    parent_state: &State,
    reward: u64,
    miner: [u8; 32],
    normal: Option<Transaction>,
    target: [u8; 32],
    timestamp: u64,
) -> Result<(Block, State), BlockError> {
    let fees = normal.as_ref().map(|t| t.fee).unwrap_or(0);
    let coinbase_amount = reward.checked_add(fees).ok_or(BlockError::CoinbaseTooLarge)?;
    let coinbase = Transaction::coinbase(miner, coinbase_amount);

    let mut transactions = vec![coinbase];
    if let Some(tx) = normal {
        transactions.push(tx);
    }

    // Apply normals then coinbase to get the resulting state and its root.
    let mut state = parent_state.clone();
    for tx in &transactions[1..] {
        if !tx.verify_signature() {
            return Err(BlockError::BadSignature);
        }
        state.apply(tx).map_err(BlockError::Apply)?;
    }
    state.apply(&transactions[0]).map_err(BlockError::Apply)?;

    let mut header = BlockHeader {
        version: BLOCK_VERSION,
        height: parent.height + 1,
        previous_block_hash: parent.block_hash(),
        transactions_hash: transactions_hash(&transactions),
        state_root: state.state_root(),
        timestamp,
        target,
        nonce: 0,
    };

    loop {
        if header.meets_target() {
            break;
        }
        header.nonce = header.nonce.wrapping_add(1);
    }

    Ok((
        Block {
            header,
            transactions,
        },
        state,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};
    use rand::rngs::OsRng;

    #[test]
    fn genesis_marker_is_32_bytes() {
        assert_eq!(GENESIS_MARKER.len(), 32);
    }

    #[test]
    fn header_round_trips() {
        let h = Block::genesis().header;
        assert_eq!(BlockHeader::from_bytes(&h.serialize()), Some(h));
    }

    #[test]
    fn mined_block_validates_and_pays_the_miner() {
        let genesis = Block::genesis();
        let state = State::new();
        let miner = [7u8; 32];
        let (block, new_state) =
            build_and_mine(&genesis.header, &state, BLOCK_REWARD, miner, None, STATIC_TARGET, 1)
                .unwrap();

        assert!(block.header.meets_target());
        let applied = block
            .validate_and_apply(&genesis.header, &state, BLOCK_REWARD, &STATIC_TARGET)
            .unwrap();
        assert_eq!(applied.balance(&miner), BLOCK_REWARD);
        assert_eq!(new_state.balance(&miner), BLOCK_REWARD);
    }

    #[test]
    fn block_with_a_transfer_validates() {
        // Block 1 mints to a sender; block 2 includes a transfer from them.
        let genesis = Block::genesis();
        let s0 = State::new();
        let key = SigningKey::generate(&mut OsRng);
        let sender = key.verifying_key().to_bytes();

        let (b1, s1) =
            build_and_mine(&genesis.header, &s0, BLOCK_REWARD, sender, None, STATIC_TARGET, 1)
                .unwrap();

        let mut tx = Transaction {
            tx_type: crate::tx_type::NORMAL,
            sender,
            recipient: [9u8; 32],
            amount: 10,
            fee: 2,
            nonce: 0,
            signature: [0u8; 64],
        };
        tx.signature = key.sign(&tx.signing_bytes()).to_bytes();

        let (b2, s2) = build_and_mine(
            &b1.header,
            &s1,
            BLOCK_REWARD,
            [8u8; 32],
            Some(tx),
            STATIC_TARGET,
            2,
        )
        .unwrap();

        b2.validate_and_apply(&b1.header, &s1, BLOCK_REWARD, &STATIC_TARGET)
            .unwrap();
        assert_eq!(s2.balance(&[9u8; 32]), 10);
        // Miner of block 2 gets reward + fee.
        assert_eq!(s2.balance(&[8u8; 32]), BLOCK_REWARD + 2);
        // Sender spent amount + fee.
        assert_eq!(s2.balance(&sender), BLOCK_REWARD - 12);
    }

    #[test]
    fn tampered_state_root_is_rejected() {
        let genesis = Block::genesis();
        let state = State::new();
        let (mut block, _) =
            build_and_mine(&genesis.header, &state, BLOCK_REWARD, [1u8; 32], None, STATIC_TARGET, 1)
                .unwrap();
        block.header.state_root = [0u8; 32];
        // Re-mine so the PoW still holds on the tampered header.
        while !block.header.meets_target() {
            block.header.nonce = block.header.nonce.wrapping_add(1);
        }
        let err = block
            .validate_and_apply(&genesis.header, &state, BLOCK_REWARD, &STATIC_TARGET)
            .unwrap_err();
        assert_eq!(err, BlockError::BadStateRoot);
    }

    #[test]
    fn oversized_coinbase_is_rejected() {
        let genesis = Block::genesis();
        let state = State::new();
        let coinbase = Transaction::coinbase([1u8; 32], BLOCK_REWARD + 1);
        let transactions = vec![coinbase];
        let mut applied = state.clone();
        applied.apply(&transactions[0]).unwrap();
        let mut header = BlockHeader {
            version: BLOCK_VERSION,
            height: 1,
            previous_block_hash: genesis.header.block_hash(),
            transactions_hash: transactions_hash(&transactions),
            state_root: applied.state_root(),
            timestamp: 1,
            target: STATIC_TARGET,
            nonce: 0,
        };
        while header.block_hash() >= header.target {
            header.nonce += 1;
        }
        let block = Block { header, transactions };
        let err = block
            .validate_and_apply(&genesis.header, &state, BLOCK_REWARD, &STATIC_TARGET)
            .unwrap_err();
        assert_eq!(err, BlockError::CoinbaseTooLarge);
    }
}
