//! Blocks, the 156-byte header, proof of work, genesis, mining, and validation.
//!
//! A v1 block body is a coinbase first, then zero or one normal transaction. The
//! header commits to the transactions and to the resulting account state. Proof
//! of work is `SHA256(header) < target`, compared as big-endian 256-bit numbers.

use sha2::{Digest, Sha256};

use crate::consensus::{BlockContext, Params};
use crate::state::{State, StateError};
use crate::Transaction;

/// Header size in bytes: 4 + 8 + 32 + 32 + 32 + 8 + 32 + 8.
pub const HEADER_LEN: usize = 156;

/// The block/consensus version this binary produces. Validation requires the
/// version that `consensus::consensus_version(height)` selects (via
/// `BlockContext::version`), so a future rule change activates by height without
/// a restart. v2 is the current (genesis) rule set.
pub const BLOCK_VERSION: u32 = crate::consensus::LATEST_VERSION;

/// Maximum number of normal transactions in a block body (besides the coinbase).
/// A v2 body is a coinbase followed by up to `MAX_TXS` normal transactions.
pub const MAX_TXS: usize = 64;

/// The 32-byte plaintext marker placed in the genesis `previous_block_hash`.
pub const GENESIS_MARKER: &[u8; 32] = b"I am 21 years old, I love Jeffy!";

/// A block timestamp may not be more than this many seconds ahead of the
/// receiving node's clock. This is a local admission rule, not consensus: a
/// block briefly in the future is accepted once the clock catches up, so it is
/// enforced on receipt, never during deterministic validation or replay.
///
/// It is 10 minutes, shorter than the real network's `stall_grace`
/// (20 minutes), so that a timestamp set ahead of the clock cannot claim the
/// stall fallback's easier target (see `consensus::eased_target`). A node
/// whose clock is more than 10 minutes slow will refuse current blocks.
pub const MAX_FUTURE_DRIFT: u64 = 10 * 60;

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
    /// `header.version` is not a version this node implements.
    BadVersion,
    BadHeight,
    BadParentHash,
    WrongTarget,
    PowNotMet,
    BadBody,
    /// The coinbase has a nonzero sender, fee, nonce, or signature.
    BadCoinbase,
    BadTransactionsHash,
    BadStateRoot,
    CoinbaseTooLarge,
    BadSignature,
    /// Timestamp is not strictly greater than the median of recent blocks.
    TimestampTooOld,
    Apply(StateError),
}

impl std::fmt::Display for BlockError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BlockError::BadVersion => write!(f, "unknown block version"),
            BlockError::BadHeight => write!(f, "height is not parent height + 1"),
            BlockError::BadParentHash => write!(f, "previous_block_hash does not match parent"),
            BlockError::WrongTarget => write!(f, "target does not match consensus"),
            BlockError::PowNotMet => write!(f, "block hash does not meet the target"),
            BlockError::BadBody => write!(f, "body is not a coinbase plus up to MAX_TXS normal txs"),
            BlockError::BadCoinbase => write!(f, "coinbase is not in canonical form"),
            BlockError::BadTransactionsHash => write!(f, "transactions_hash mismatch"),
            BlockError::BadStateRoot => write!(f, "state_root mismatch"),
            BlockError::CoinbaseTooLarge => write!(f, "coinbase exceeds reward plus fees"),
            BlockError::BadSignature => write!(f, "a normal transaction has an invalid signature"),
            BlockError::TimestampTooOld => {
                write!(f, "timestamp is not greater than the median of recent blocks")
            }
            BlockError::Apply(e) => write!(f, "state apply failed: {e}"),
        }
    }
}

impl std::error::Error for BlockError {}

impl Block {
    /// The genesis block of a parameter set. All nodes on one network trust the
    /// same genesis; it is not proof-of-work validated. Height 0, empty body,
    /// empty state.
    pub fn genesis(params: &Params) -> Block {
        let transactions: Vec<Transaction> = Vec::new();
        let header = BlockHeader {
            version: BLOCK_VERSION,
            height: 0,
            previous_block_hash: *GENESIS_MARKER,
            transactions_hash: transactions_hash(&transactions),
            state_root: State::new().state_root(),
            timestamp: params.genesis_timestamp,
            target: params.pow_limit,
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

    /// Wire serialization: header (156) | tx count (u16) | transactions (153 each).
    pub fn serialize(&self) -> Vec<u8> {
        let mut bytes = self.header.serialize();
        let count = self.transactions.len().min(u16::MAX as usize) as u16;
        bytes.extend_from_slice(&count.to_be_bytes());
        for tx in self.transactions.iter().take(count as usize) {
            bytes.extend_from_slice(&tx.serialize_to_bytes());
        }
        bytes
    }

    /// Parse a block from its wire serialization.
    pub fn from_bytes(buf: &[u8]) -> Option<Block> {
        if buf.len() < HEADER_LEN + 2 {
            return None;
        }
        let header = BlockHeader::from_bytes(&buf[0..HEADER_LEN])?;
        let count = u16::from_be_bytes(buf[HEADER_LEN..HEADER_LEN + 2].try_into().ok()?) as usize;
        let mut pos = HEADER_LEN + 2;
        let mut transactions = Vec::with_capacity(count);
        for _ in 0..count {
            let end = pos.checked_add(Transaction::SERIALIZED_LEN)?;
            transactions.push(Transaction::from_bytes(buf.get(pos..end)?)?);
            pos = end;
        }
        if pos != buf.len() {
            return None; // trailing bytes or a lying count
        }
        Some(Block {
            header,
            transactions,
        })
    }

    /// Split the body into the coinbase and the normal transactions, enforcing
    /// the v2 shape: first is a coinbase, the rest are normal, up to `MAX_TXS`.
    fn split_body(&self) -> Result<(&Transaction, &[Transaction]), BlockError> {
        let first = self.transactions.first().ok_or(BlockError::BadBody)?;
        if !first.is_coinbase() {
            return Err(BlockError::BadBody);
        }
        if !first.is_canonical_coinbase() {
            return Err(BlockError::BadCoinbase);
        }
        let rest = &self.transactions[1..];
        // Every other transaction must be a known, normal type: an unknown type
        // is rejected, never treated as a transfer.
        if rest.len() > MAX_TXS || rest.iter().any(|t| !t.is_normal()) {
            return Err(BlockError::BadBody);
        }
        Ok((first, rest))
    }

    /// Validate this block against its parent header, the parent's state, and
    /// the consensus context for a child of that parent (see
    /// `Chain::context_for_child`), and return the resulting state. This is the
    /// whole deterministic rule set: version, height, parent link, target,
    /// minimum timestamp, proof of work, body shape, signatures, reward, and
    /// state root. The normal transactions are applied first, then the
    /// coinbase, so a miner cannot spend the reward within the same block.
    pub fn validate_and_apply(
        &self,
        parent: &BlockHeader,
        parent_state: &State,
        ctx: &BlockContext,
    ) -> Result<State, BlockError> {
        if self.header.version != ctx.version {
            return Err(BlockError::BadVersion);
        }
        if self.header.height != parent.height + 1 || self.header.height != ctx.height {
            return Err(BlockError::BadHeight);
        }
        if self.header.previous_block_hash != parent.block_hash() {
            return Err(BlockError::BadParentHash);
        }
        if self.header.target != ctx.target_at(self.header.timestamp) {
            return Err(BlockError::WrongTarget);
        }
        if self.header.timestamp < ctx.min_timestamp {
            return Err(BlockError::TimestampTooOld);
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
        let max_coinbase = ctx.reward.checked_add(fees).ok_or(BlockError::CoinbaseTooLarge)?;
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

/// Assemble the next block on top of `parent` without searching for a nonce.
///
/// Builds the coinbase (reward plus the normal transaction's fee) to `miner`,
/// applies the body to the parent state, and returns a header with `nonce = 0`
/// plus the body and resulting state. The caller searches nonces (all at once
/// with `build_and_mine`, or in chunks with `mine_chunk` so mining can be
/// abandoned when the chain moves).
pub fn assemble_block(
    parent: &BlockHeader,
    parent_state: &State,
    reward: u64,
    miner: [u8; 32],
    normal: Option<Transaction>,
    target: [u8; 32],
    timestamp: u64,
) -> Result<(BlockHeader, Vec<Transaction>, State), BlockError> {
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

    let header = BlockHeader {
        version: BLOCK_VERSION,
        height: parent.height + 1,
        previous_block_hash: parent.block_hash(),
        transactions_hash: transactions_hash(&transactions),
        state_root: state.state_root(),
        timestamp,
        target,
        nonce: 0,
    };

    Ok((header, transactions, state))
}

/// Try `attempts` nonces on `header`, advancing `header.nonce`. Returns true and
/// leaves `header.nonce` at the winning value if one meets the target, else
/// false with the nonce advanced past the tried range. Lets a caller mine in
/// bounded chunks and check between them whether to keep going.
pub fn mine_chunk(header: &mut BlockHeader, attempts: u64) -> bool {
    for _ in 0..attempts {
        if header.meets_target() {
            return true;
        }
        header.nonce = header.nonce.wrapping_add(1);
    }
    false
}

/// Assemble and fully mine the next block on top of `parent`.
pub fn build_and_mine(
    parent: &BlockHeader,
    parent_state: &State,
    reward: u64,
    miner: [u8; 32],
    normal: Option<Transaction>,
    target: [u8; 32],
    timestamp: u64,
) -> Result<(Block, State), BlockError> {
    let (mut header, transactions, state) =
        assemble_block(parent, parent_state, reward, miner, normal, target, timestamp)?;
    while !header.meets_target() {
        header.nonce = header.nonce.wrapping_add(1);
    }
    Ok((Block { header, transactions }, state))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consensus::REGTEST;
    use ed25519_dalek::{Signer, SigningKey};
    use rand::rngs::OsRng;

    /// Validate `block` with a context built from explicit values: height
    /// parent + 1, the given reward and target, and no timestamp floor. Lets
    /// each test vary one input; the real context comes from `Chain`.
    fn validate_with(
        block: &Block,
        parent: &BlockHeader,
        state: &State,
        reward: u64,
        target: &[u8; 32],
    ) -> Result<State, BlockError> {
        let ctx = BlockContext {
            height: parent.height + 1,
            version: crate::consensus::consensus_version(parent.height + 1),
            reward,
            target: *target,
            min_timestamp: 0,
            parent_timestamp: 0,
            stall_grace: 0,
            stall_step: 0,
            pow_limit: *target,
        };
        block.validate_and_apply(parent, state, &ctx)
    }

    #[test]
    fn genesis_marker_is_32_bytes() {
        assert_eq!(GENESIS_MARKER.len(), 32);
    }

    #[test]
    fn header_round_trips() {
        let h = Block::genesis(&REGTEST).header;
        assert_eq!(BlockHeader::from_bytes(&h.serialize()), Some(h));
    }

    #[test]
    fn block_round_trips() {
        let genesis = Block::genesis(&REGTEST);
        let state = State::new();
        let (block, _) =
            build_and_mine(&genesis.header, &state, REGTEST.initial_reward, [3u8; 32], None, REGTEST.pow_limit, 1)
                .unwrap();
        let decoded = Block::from_bytes(&block.serialize()).unwrap();
        assert_eq!(decoded.header, block.header);
        assert_eq!(decoded.transactions.len(), block.transactions.len());
    }

    #[test]
    fn mined_block_validates_and_pays_the_miner() {
        let genesis = Block::genesis(&REGTEST);
        let state = State::new();
        let miner = [7u8; 32];
        let (block, new_state) =
            build_and_mine(&genesis.header, &state, REGTEST.initial_reward, miner, None, REGTEST.pow_limit, 1)
                .unwrap();

        assert!(block.header.meets_target());
        let applied = validate_with(&block, &genesis.header, &state, REGTEST.initial_reward, &REGTEST.pow_limit)
            .unwrap();
        assert_eq!(applied.balance(&miner), REGTEST.initial_reward);
        assert_eq!(new_state.balance(&miner), REGTEST.initial_reward);
    }

    #[test]
    fn block_with_a_transfer_validates() {
        // Block 1 mints to a sender; block 2 includes a transfer from them.
        let genesis = Block::genesis(&REGTEST);
        let s0 = State::new();
        let key = SigningKey::generate(&mut OsRng);
        let sender = key.verifying_key().to_bytes();

        let (b1, s1) =
            build_and_mine(&genesis.header, &s0, REGTEST.initial_reward, sender, None, REGTEST.pow_limit, 1)
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
            REGTEST.initial_reward,
            [8u8; 32],
            Some(tx),
            REGTEST.pow_limit,
            2,
        )
        .unwrap();

        validate_with(&b2, &b1.header, &s1, REGTEST.initial_reward, &REGTEST.pow_limit)
            .unwrap();
        assert_eq!(s2.balance(&[9u8; 32]), 10);
        // Miner of block 2 gets reward + fee.
        assert_eq!(s2.balance(&[8u8; 32]), REGTEST.initial_reward + 2);
        // Sender spent amount + fee.
        assert_eq!(s2.balance(&sender), REGTEST.initial_reward - 12);
    }

    #[test]
    fn tampered_state_root_is_rejected() {
        let genesis = Block::genesis(&REGTEST);
        let state = State::new();
        let (mut block, _) =
            build_and_mine(&genesis.header, &state, REGTEST.initial_reward, [1u8; 32], None, REGTEST.pow_limit, 1)
                .unwrap();
        block.header.state_root = [0u8; 32];
        // Re-mine so the PoW still holds on the tampered header.
        while !block.header.meets_target() {
            block.header.nonce = block.header.nonce.wrapping_add(1);
        }
        let err = validate_with(&block, &genesis.header, &state, REGTEST.initial_reward, &REGTEST.pow_limit)
            .unwrap_err();
        assert_eq!(err, BlockError::BadStateRoot);
    }

    #[test]
    fn oversized_coinbase_is_rejected() {
        let genesis = Block::genesis(&REGTEST);
        let state = State::new();
        let coinbase = Transaction::coinbase([1u8; 32], REGTEST.initial_reward + 1);
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
            target: REGTEST.pow_limit,
            nonce: 0,
        };
        while header.block_hash() >= header.target {
            header.nonce += 1;
        }
        let block = Block { header, transactions };
        let err = validate_with(&block, &genesis.header, &state, REGTEST.initial_reward, &REGTEST.pow_limit)
            .unwrap_err();
        assert_eq!(err, BlockError::CoinbaseTooLarge);
    }

    // --- Reward schedule (halving). ---

    #[test]
    fn timestamp_below_the_context_minimum_is_rejected() {
        let genesis = Block::genesis(&REGTEST);
        let state = State::new();
        let (block, _) = build_and_mine(
            &genesis.header,
            &state,
            REGTEST.initial_reward,
            [1u8; 32],
            None,
            REGTEST.pow_limit,
            50,
        )
        .unwrap();
        let ctx = |min_timestamp| BlockContext {
            height: 1,
            version: BLOCK_VERSION,
            reward: REGTEST.initial_reward,
            target: REGTEST.pow_limit,
            min_timestamp,
            parent_timestamp: 0,
            stall_grace: 0,
            stall_step: 0,
            pow_limit: [0xff; 32],
        };
        assert!(block.validate_and_apply(&genesis.header, &state, &ctx(50)).is_ok());
        assert_eq!(
            block.validate_and_apply(&genesis.header, &state, &ctx(51)).unwrap_err(),
            BlockError::TimestampTooOld
        );
    }

    #[test]
    fn coinbase_is_capped_by_the_halved_reward() {
        // A parent one block before the first halving, so the child is the
        // first block of the halved era.
        let mut parent = Block::genesis(&REGTEST).header;
        parent.height = REGTEST.halving_interval - 1;
        let state = State::new();
        let halved = REGTEST.block_reward(REGTEST.halving_interval);

        // Claiming the old reward is rejected.
        let (greedy, _) =
            build_and_mine(&parent, &state, REGTEST.initial_reward, [1u8; 32], None, REGTEST.pow_limit, 1)
                .unwrap();
        assert_eq!(greedy.header.height, REGTEST.halving_interval);
        let err = validate_with(&greedy, &parent, &state, halved, &REGTEST.pow_limit)
            .unwrap_err();
        assert_eq!(err, BlockError::CoinbaseTooLarge);

        // Claiming the halved reward is accepted and pays exactly that.
        let (honest, _) =
            build_and_mine(&parent, &state, halved, [2u8; 32], None, REGTEST.pow_limit, 1).unwrap();
        let applied = validate_with(&honest, &parent, &state, halved, &REGTEST.pow_limit)
            .unwrap();
        assert_eq!(applied.balance(&[2u8; 32]), REGTEST.initial_reward / 2);
    }

    // --- Adversarial battery: each malformed block must be rejected. ---

    fn mine_header(header: &mut BlockHeader) {
        while !header.meets_target() {
            header.nonce = header.nonce.wrapping_add(1);
        }
    }

    #[test]
    fn unmined_block_is_rejected() {
        let genesis = Block::genesis(&REGTEST);
        let (mut block, _) =
            build_and_mine(&genesis.header, &State::new(), REGTEST.initial_reward, [7u8; 32], None, REGTEST.pow_limit, 1)
                .unwrap();
        // Break the proof of work.
        while block.header.meets_target() {
            block.header.nonce = block.header.nonce.wrapping_add(1);
        }
        let err = validate_with(&block, &genesis.header, &State::new(), REGTEST.initial_reward, &REGTEST.pow_limit)
            .unwrap_err();
        assert_eq!(err, BlockError::PowNotMet);
    }

    #[test]
    fn wrong_target_is_rejected() {
        let genesis = Block::genesis(&REGTEST);
        let (mut block, _) =
            build_and_mine(&genesis.header, &State::new(), REGTEST.initial_reward, [7u8; 32], None, REGTEST.pow_limit, 1)
                .unwrap();
        block.header.target = [0xff; 32]; // claim a trivial target
        let err = validate_with(&block, &genesis.header, &State::new(), REGTEST.initial_reward, &REGTEST.pow_limit)
            .unwrap_err();
        assert_eq!(err, BlockError::WrongTarget);
    }

    #[test]
    fn bad_transactions_hash_is_rejected() {
        let genesis = Block::genesis(&REGTEST);
        let (mut block, _) =
            build_and_mine(&genesis.header, &State::new(), REGTEST.initial_reward, [7u8; 32], None, REGTEST.pow_limit, 1)
                .unwrap();
        block.header.transactions_hash = [0u8; 32];
        mine_header(&mut block.header);
        let err = validate_with(&block, &genesis.header, &State::new(), REGTEST.initial_reward, &REGTEST.pow_limit)
            .unwrap_err();
        assert_eq!(err, BlockError::BadTransactionsHash);
    }

    #[test]
    fn body_without_a_coinbase_is_rejected() {
        let genesis = Block::genesis(&REGTEST);
        let normal = Transaction {
            tx_type: crate::tx_type::NORMAL,
            sender: [1u8; 32],
            recipient: [2u8; 32],
            amount: 1,
            fee: 0,
            nonce: 0,
            signature: [0u8; 64],
        };
        let transactions = vec![normal];
        let mut header = BlockHeader {
            version: BLOCK_VERSION,
            height: 1,
            previous_block_hash: genesis.header.block_hash(),
            transactions_hash: transactions_hash(&transactions),
            state_root: State::new().state_root(),
            timestamp: 1,
            target: REGTEST.pow_limit,
            nonce: 0,
        };
        mine_header(&mut header);
        let block = Block { header, transactions };
        let err = validate_with(&block, &genesis.header, &State::new(), REGTEST.initial_reward, &REGTEST.pow_limit)
            .unwrap_err();
        assert_eq!(err, BlockError::BadBody);
    }

    /// Mine a height-1 block on genesis with exactly these transactions and this
    /// version. The state root is computed by applying the body as `apply` would,
    /// so the only defect is the one under test.
    fn block_on_genesis(transactions: Vec<Transaction>, version: u32) -> Block {
        let genesis = Block::genesis(&REGTEST);
        let mut state = State::new();
        for tx in &transactions {
            let _ = state.apply(tx);
        }
        let mut header = BlockHeader {
            version,
            height: 1,
            previous_block_hash: genesis.header.block_hash(),
            transactions_hash: transactions_hash(&transactions),
            state_root: state.state_root(),
            timestamp: 1,
            target: REGTEST.pow_limit,
            nonce: 0,
        };
        mine_header(&mut header);
        Block { header, transactions }
    }

    fn validate_on_genesis(block: &Block) -> Result<State, BlockError> {
        validate_with(block, &Block::genesis(&REGTEST).header, &State::new(), REGTEST.initial_reward, &REGTEST.pow_limit)
    }

    #[test]
    fn unknown_block_version_is_rejected() {
        let coinbase = Transaction::coinbase([1u8; 32], REGTEST.initial_reward);
        let block = block_on_genesis(vec![coinbase.clone()], BLOCK_VERSION + 1);
        assert_eq!(validate_on_genesis(&block).unwrap_err(), BlockError::BadVersion);
        // Control: the same body at the current version is valid.
        let block = block_on_genesis(vec![coinbase], BLOCK_VERSION);
        assert!(validate_on_genesis(&block).is_ok());
    }

    #[test]
    fn non_canonical_coinbase_is_rejected() {
        // Each variant sets one ignored coinbase field to a nonzero value.
        let mut variants = Vec::new();
        let mut cb = Transaction::coinbase([1u8; 32], REGTEST.initial_reward);
        cb.sender = [5u8; 32];
        variants.push(cb);
        let mut cb = Transaction::coinbase([1u8; 32], REGTEST.initial_reward);
        cb.fee = 1;
        variants.push(cb);
        let mut cb = Transaction::coinbase([1u8; 32], REGTEST.initial_reward);
        cb.nonce = 7;
        variants.push(cb);
        let mut cb = Transaction::coinbase([1u8; 32], REGTEST.initial_reward);
        cb.signature = [9u8; 64];
        variants.push(cb);
        for cb in variants {
            let block = block_on_genesis(vec![cb], BLOCK_VERSION);
            assert_eq!(validate_on_genesis(&block).unwrap_err(), BlockError::BadCoinbase);
        }
    }

    #[test]
    fn unknown_transaction_type_in_body_is_rejected() {
        let coinbase = Transaction::coinbase([1u8; 32], REGTEST.initial_reward);
        let unknown = Transaction {
            tx_type: 7,
            sender: [1u8; 32],
            recipient: [2u8; 32],
            amount: 1,
            fee: 0,
            nonce: 0,
            signature: [0u8; 64],
        };
        let block = block_on_genesis(vec![coinbase, unknown], BLOCK_VERSION);
        assert_eq!(validate_on_genesis(&block).unwrap_err(), BlockError::BadBody);
    }

    #[test]
    fn unknown_transaction_type_does_not_decode() {
        let mut bytes = Transaction::coinbase([1u8; 32], 5).serialize_to_bytes();
        assert!(Transaction::from_bytes(&bytes).is_some());
        bytes[0] = 2;
        assert!(Transaction::from_bytes(&bytes).is_none());
        bytes[0] = 255;
        assert!(Transaction::from_bytes(&bytes).is_none());
    }

    #[test]
    fn bad_signature_in_block_is_rejected() {
        let genesis = Block::genesis(&REGTEST);
        let key = SigningKey::generate(&mut OsRng);
        let coinbase = Transaction::coinbase([7u8; 32], REGTEST.initial_reward);
        let normal = Transaction {
            tx_type: crate::tx_type::NORMAL,
            sender: key.verifying_key().to_bytes(),
            recipient: [2u8; 32],
            amount: 1,
            fee: 0,
            nonce: 0,
            signature: [0u8; 64], // never signed
        };
        let transactions = vec![coinbase, normal];
        let mut header = BlockHeader {
            version: BLOCK_VERSION,
            height: 1,
            previous_block_hash: genesis.header.block_hash(),
            transactions_hash: transactions_hash(&transactions),
            state_root: State::new().state_root(),
            timestamp: 1,
            target: REGTEST.pow_limit,
            nonce: 0,
        };
        mine_header(&mut header);
        let block = Block { header, transactions };
        let err = validate_with(&block, &genesis.header, &State::new(), REGTEST.initial_reward, &REGTEST.pow_limit)
            .unwrap_err();
        assert_eq!(err, BlockError::BadSignature);
    }

    #[test]
    fn overspend_in_block_is_rejected() {
        let genesis = Block::genesis(&REGTEST);
        let key = SigningKey::generate(&mut OsRng);
        let coinbase = Transaction::coinbase([7u8; 32], REGTEST.initial_reward);
        let mut normal = Transaction {
            tx_type: crate::tx_type::NORMAL,
            sender: key.verifying_key().to_bytes(),
            recipient: [2u8; 32],
            amount: 10, // sender has no funds
            fee: 0,
            nonce: 0,
            signature: [0u8; 64],
        };
        normal.signature = key.sign(&normal.signing_bytes()).to_bytes(); // valid signature
        let transactions = vec![coinbase, normal];
        let mut header = BlockHeader {
            version: BLOCK_VERSION,
            height: 1,
            previous_block_hash: genesis.header.block_hash(),
            transactions_hash: transactions_hash(&transactions),
            state_root: State::new().state_root(),
            timestamp: 1,
            target: REGTEST.pow_limit,
            nonce: 0,
        };
        mine_header(&mut header);
        let block = Block { header, transactions };
        let err = validate_with(&block, &genesis.header, &State::new(), REGTEST.initial_reward, &REGTEST.pow_limit)
            .unwrap_err();
        assert!(matches!(err, BlockError::Apply(_)));
    }
}
