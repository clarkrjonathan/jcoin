//! The block store and canonical-chain selection.
//!
//! Blocks are kept by hash, each with its height, cumulative work, and the
//! account state after applying it. The canonical tip is the stored block with
//! the greatest cumulative work (not the greatest height), so competing forks
//! are retained and a reorg is just moving the tip pointer. Each block's
//! required difficulty target and its timestamp lower bound (median-time-past)
//! are computed here from the block's own ancestry.

use std::collections::HashMap;

use primitive_types::U256;

use crate::block::{Block, BlockError};
use crate::consensus::{BlockContext, Params};
use crate::state::State;

/// New target from a parent target and the measured vs expected span of a
/// retarget window. The span is clamped to `[expected/max_adjust_factor,
/// expected*max_adjust_factor]` so difficulty moves at most that factor per
/// step; the result is never easier than `pow_limit` nor zero. A span shorter
/// than expected (blocks too fast) shrinks the target (harder).
fn retarget(
    params: &Params,
    parent_target: &[u8; 32],
    actual_span: u64,
    expected_span: u64,
) -> [u8; 32] {
    let expected = expected_span.max(1);
    let factor = params.max_adjust_factor;
    let clamped = actual_span.clamp(expected / factor, expected * factor).max(1);
    let parent = U256::from_big_endian(parent_target);
    let new_target = parent.saturating_mul(U256::from(clamped)) / U256::from(expected);
    let capped = new_target
        .min(U256::from_big_endian(&params.pow_limit))
        .max(U256::one());
    let mut bytes = [0u8; 32];
    capped.to_big_endian(&mut bytes);
    bytes
}

/// Work contributed by a block with the given target: `2^256 / (target + 1)`.
/// A smaller target (harder) yields more work.
pub fn block_work(target: &[u8; 32]) -> U256 {
    let t = U256::from_big_endian(target);
    match t.checked_add(U256::one()) {
        Some(denom) => U256::MAX / denom,
        None => U256::one(), // target was the maximum; minimal work
    }
}

/// A block as stored, with the metadata needed for fork choice and validation.
struct StoredBlock {
    block: Block,
    height: u64,
    work: U256,
    state: State,
}

/// Outcome of offering a block to the store.
pub enum Accept {
    /// Newly stored and connected to a known parent. `new_tip` is true if it
    /// became the canonical tip.
    Stored { new_tip: bool, height: u64 },
    /// Already in the store.
    AlreadyHave { height: u64 },
    /// Parent is unknown; the caller should sync the gap.
    Orphan,
    /// Connected to a parent but failed validation.
    Invalid(BlockError),
}

pub struct Chain {
    params: Params,
    blocks: HashMap<[u8; 32], StoredBlock>,
    tip: [u8; 32],
}

impl Chain {
    /// A new chain for `params`, holding only its genesis block.
    pub fn new(params: Params) -> Self {
        let genesis = Block::genesis(&params);
        let hash = genesis.hash();
        let mut blocks = HashMap::new();
        blocks.insert(
            hash,
            StoredBlock {
                block: genesis,
                height: 0,
                work: U256::zero(),
                state: State::new(),
            },
        );
        Chain {
            params,
            blocks,
            tip: hash,
        }
    }

    /// The consensus parameters this chain validates against.
    pub fn params(&self) -> &Params {
        &self.params
    }

    pub fn tip_hash(&self) -> [u8; 32] {
        self.tip
    }

    pub fn tip_height(&self) -> u64 {
        self.blocks[&self.tip].height
    }

    pub fn tip_header(&self) -> crate::block::BlockHeader {
        self.blocks[&self.tip].block.header.clone()
    }

    pub fn tip_state(&self) -> &State {
        &self.blocks[&self.tip].state
    }

    pub fn contains(&self, hash: &[u8; 32]) -> bool {
        self.blocks.contains_key(hash)
    }

    /// The consensus context for the next block on the current tip.
    pub fn next_context(&self) -> BlockContext {
        self.context_for_child(self.tip)
    }

    /// Everything consensus requires of a child of `parent_hash`: its height,
    /// the subsidy it may claim, the target it must carry, and its minimum
    /// timestamp. This is the one place these inputs are derived; validation
    /// (`accept`) and mining (`next_context`) both use it.
    ///
    /// Panics if `parent_hash` is not stored; callers check first.
    pub fn context_for_child(&self, parent_hash: [u8; 32]) -> BlockContext {
        let height = self.blocks[&parent_hash].height + 1;
        BlockContext {
            height,
            reward: self.params.block_reward(height),
            target: self.target_for_child(parent_hash),
            // Strictly greater than median-time-past.
            min_timestamp: self.median_time_past(parent_hash).saturating_add(1),
        }
    }

    /// The `n`-th ancestor of the block at `start` (0 = that block itself).
    /// `None` if the walk runs past genesis.
    fn ancestor(&self, start: [u8; 32], n: u64) -> Option<&StoredBlock> {
        let mut cur = self.blocks.get(&start)?;
        for _ in 0..n {
            cur = self.blocks.get(&cur.block.header.previous_block_hash)?;
        }
        Some(cur)
    }

    /// Median timestamp of up to `median_time_span` blocks ending at `parent`.
    /// A new block's timestamp must be strictly greater than this.
    fn median_time_past(&self, parent_hash: [u8; 32]) -> u64 {
        let span = self.params.median_time_span;
        let mut times = Vec::with_capacity(span);
        let mut cur = self.blocks.get(&parent_hash);
        while let Some(b) = cur {
            times.push(b.block.header.timestamp);
            if times.len() >= span {
                break;
            }
            cur = self.blocks.get(&b.block.header.previous_block_hash);
        }
        times.sort_unstable();
        times[times.len() / 2]
    }

    /// The required difficulty target for the child of `parent_hash`.
    ///
    /// Epoch-based: difficulty is constant within an epoch of `RETARGET_WINDOW`
    /// blocks and is recomputed only at an epoch boundary, from the span of the
    /// epoch just completed. Adjusting only after observing a full epoch at the
    /// current difficulty (rather than every block off a trailing window) is
    /// what keeps it from overshooting. Steered toward `TARGET_BLOCK_TIME`,
    /// clamped per step, never easier than `POW_LIMIT`.
    fn target_for_child(&self, parent_hash: [u8; 32]) -> [u8; 32] {
        let p = &self.params;
        let parent = &self.blocks[&parent_hash];
        let height = parent.height + 1;

        // First epoch mines at the easy limit.
        if height < p.retarget_window {
            return p.pow_limit;
        }
        // Mid-epoch: keep the epoch's difficulty (inherit the parent's target).
        if height % p.retarget_window != 0 {
            return parent.block.header.target;
        }

        // Epoch boundary: retarget from the span of the just-completed epoch,
        // blocks [height - retarget_window, height - 1].
        let intervals = p.retarget_window - 1;
        let Some(first) = self.ancestor(parent_hash, intervals) else {
            return p.pow_limit;
        };
        let expected = intervals * p.target_block_time;
        let actual = parent
            .block
            .header
            .timestamp
            .saturating_sub(first.block.header.timestamp);
        retarget(p, &parent.block.header.target, actual, expected)
    }

    /// Offer a block to the store: validate against its parent, store it, and
    /// switch the tip if it now has the greatest cumulative work.
    pub fn accept(&mut self, block: Block) -> Accept {
        let hash = block.hash();
        if let Some(stored) = self.blocks.get(&hash) {
            return Accept::AlreadyHave {
                height: stored.height,
            };
        }

        let parent_hash = block.header.previous_block_hash;
        if !self.blocks.contains_key(&parent_hash) {
            return Accept::Orphan;
        }

        let ctx = self.context_for_child(parent_hash);
        let parent = &self.blocks[&parent_hash];
        let new_state = match block.validate_and_apply(&parent.block.header, &parent.state, &ctx) {
            Ok(s) => s,
            Err(e) => return Accept::Invalid(e),
        };

        let height = parent.height + 1;
        let work = parent.work + block_work(&block.header.target);
        let new_tip = work > self.blocks[&self.tip].work;

        self.blocks.insert(
            hash,
            StoredBlock {
                block,
                height,
                work,
                state: new_state,
            },
        );
        if new_tip {
            self.tip = hash;
        }
        Accept::Stored { new_tip, height }
    }

    /// Hashes of the canonical chain from genesis up to the tip, in order.
    fn canonical_hashes(&self) -> Vec<[u8; 32]> {
        let mut path = Vec::new();
        let mut cursor = self.tip;
        loop {
            path.push(cursor);
            let stored = &self.blocks[&cursor];
            if stored.height == 0 {
                break;
            }
            cursor = stored.block.header.previous_block_hash;
        }
        path.reverse();
        path
    }

    /// A block locator: canonical tip hashes going back, dense near the tip and
    /// sparse toward genesis, always ending at genesis. A peer scans it to find
    /// the most recent common ancestor.
    pub fn locator(&self) -> Vec<[u8; 32]> {
        let path = self.canonical_hashes();
        let mut locator = Vec::new();
        let mut step: usize = 1;
        let mut i = path.len() as isize - 1;
        while i >= 0 {
            locator.push(path[i as usize]);
            if locator.len() > 10 {
                step *= 2;
            }
            i -= step as isize;
        }
        let genesis = path[0];
        if locator.last() != Some(&genesis) {
            locator.push(genesis);
        }
        locator
    }

    /// Given a peer's locator, find the first hash on our canonical chain and
    /// return up to `cap` canonical blocks after it.
    pub fn blocks_after(&self, locator: &[[u8; 32]], cap: usize) -> Vec<Block> {
        let path = self.canonical_hashes();
        let positions: HashMap<[u8; 32], usize> =
            path.iter().enumerate().map(|(i, h)| (*h, i)).collect();

        let start = locator
            .iter()
            .find_map(|h| positions.get(h).copied())
            .map(|i| i + 1)
            .unwrap_or(0);

        path.iter()
            .skip(start)
            .take(cap)
            .map(|h| self.blocks[h].block.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::{build_and_mine, Block};
    use crate::consensus::REGTEST;
    use crate::state::State;

    fn mine_on(parent: &crate::block::BlockHeader, parent_state: &State, miner: [u8; 32]) -> Block {
        build_and_mine(parent, parent_state, REGTEST.initial_reward, miner, None, REGTEST.pow_limit, 1)
            .unwrap()
            .0
    }

    #[test]
    fn accepts_a_chain_and_advances_the_tip() {
        let mut chain = Chain::new(REGTEST);
        let genesis = Block::genesis(&REGTEST);
        let b1 = mine_on(&genesis.header, &State::new(), [1u8; 32]);
        match chain.accept(b1.clone()) {
            Accept::Stored { new_tip: true, height: 1 } => {}
            _ => panic!("block 1 should extend the tip"),
        }
        assert_eq!(chain.tip_height(), 1);
        assert_eq!(chain.tip_state().balance(&[1u8; 32]), REGTEST.initial_reward);
    }

    #[test]
    fn unknown_parent_is_orphan() {
        let mut chain = Chain::new(REGTEST);
        // Mine on a fake parent the chain does not have.
        let mut fake_parent = Block::genesis(&REGTEST).header;
        fake_parent.state_root = [9u8; 32];
        let orphan = mine_on(&fake_parent, &State::new(), [1u8; 32]);
        assert!(matches!(chain.accept(orphan), Accept::Orphan));
    }

    /// Build `len` blocks on top of genesis, all crediting `miner`. Timestamps
    /// increase by height so each block clears median-time-past.
    fn branch(len: u64, miner: [u8; 32]) -> Vec<Block> {
        let mut header = Block::genesis(&REGTEST).header;
        let mut state = State::new();
        let mut out = Vec::new();
        for h in 1..=len {
            let (block, new_state) =
                build_and_mine(&header, &state, REGTEST.initial_reward, miner, None, REGTEST.pow_limit, h)
                    .unwrap();
            header = block.header.clone();
            state = new_state;
            out.push(block);
        }
        out
    }

    #[test]
    fn block_carrying_a_transfer_is_accepted() {
        use ed25519_dalek::{Signer, SigningKey};
        use rand::rngs::OsRng;

        let mut chain = Chain::new(REGTEST);
        let key = SigningKey::generate(&mut OsRng);
        let sender = key.verifying_key().to_bytes();

        // Block 1: coinbase funds the sender.
        let (b1, _) = build_and_mine(
            &Block::genesis(&REGTEST).header,
            &State::new(),
            REGTEST.initial_reward,
            sender,
            None,
            REGTEST.pow_limit,
            1,
        )
        .unwrap();
        assert!(matches!(chain.accept(b1), Accept::Stored { .. }));

        // A signed transfer from the sender, nonce 0.
        let mut tx = crate::Transaction {
            tx_type: crate::tx_type::NORMAL,
            sender,
            recipient: [2u8; 32],
            amount: 10,
            fee: 0,
            nonce: 0,
            signature: [0u8; 64],
        };
        tx.signature = key.sign(&tx.signing_bytes()).to_bytes();

        // Block 2 carries that transfer, built on the current tip.
        let (b2, _) = build_and_mine(
            &chain.tip_header(),
            chain.tip_state(),
            REGTEST.initial_reward,
            sender,
            Some(tx),
            REGTEST.pow_limit,
            2,
        )
        .unwrap();

        match chain.accept(b2) {
            Accept::Stored { height, .. } => assert_eq!(height, 2),
            Accept::Invalid(e) => panic!("block 2 with a transfer was rejected: {e}"),
            Accept::Orphan => panic!("block 2 unexpectedly orphan"),
            Accept::AlreadyHave { .. } => panic!("block 2 unexpectedly already held"),
        }
        assert_eq!(chain.tip_height(), 2);
        assert_eq!(chain.tip_state().balance(&[2u8; 32]), 10);
    }

    #[test]
    fn longer_branch_wins_by_work() {
        let mut chain = Chain::new(REGTEST);
        // A short branch to one miner, then a longer competing branch to another.
        for block in branch(2, [1u8; 32]) {
            chain.accept(block);
        }
        assert_eq!(chain.tip_height(), 2);

        for block in branch(3, [2u8; 32]) {
            chain.accept(block);
        }
        // The 3-block branch has more cumulative work and becomes canonical.
        assert_eq!(chain.tip_height(), 3);
        assert_eq!(chain.tip_state().balance(&[2u8; 32]), 3 * REGTEST.initial_reward);
        assert_eq!(chain.tip_state().balance(&[1u8; 32]), 0);
    }

    #[test]
    fn retarget_hardens_when_fast_and_caps_when_slow() {
        let expected = REGTEST.retarget_window * REGTEST.target_block_time;

        // Blocks far faster than target: the window span is tiny, so difficulty
        // tightens (target shrinks) relative to the limit.
        let faster = retarget(&REGTEST, &REGTEST.pow_limit, 1, expected);
        assert!(
            U256::from_big_endian(&faster) < U256::from_big_endian(&REGTEST.pow_limit),
            "fast blocks should harden difficulty"
        );

        // Blocks far slower than target: difficulty eases, but never past the
        // easy limit.
        let slower = retarget(&REGTEST, &faster, expected * 100, expected);
        assert!(U256::from_big_endian(&slower) > U256::from_big_endian(&faster));
        assert!(U256::from_big_endian(&slower) <= U256::from_big_endian(&REGTEST.pow_limit));
    }

    #[test]
    fn stale_timestamp_is_rejected() {
        let mut chain = Chain::new(REGTEST);
        let (b1, _) = build_and_mine(
            &Block::genesis(&REGTEST).header,
            &State::new(),
            REGTEST.initial_reward,
            [1u8; 32],
            None,
            REGTEST.pow_limit,
            100,
        )
        .unwrap();
        assert!(matches!(chain.accept(b1), Accept::Stored { .. }));

        // median-time-past of {0, 100} is 100; a block at 100 is not strictly
        // greater, so it is rejected.
        let (b2, _) = build_and_mine(
            &chain.tip_header(),
            chain.tip_state(),
            REGTEST.initial_reward,
            [1u8; 32],
            None,
            chain.next_context().target,
            100,
        )
        .unwrap();
        assert!(matches!(
            chain.accept(b2),
            Accept::Invalid(BlockError::TimestampTooOld)
        ));
    }

    /// Mine one block on the tip from the chain's own context, at `timestamp`
    /// (raised to the context minimum, as the miner does), claiming `reward`.
    fn mine_from_context(chain: &Chain, reward: u64, timestamp: u64) -> Block {
        let ctx = chain.next_context();
        build_and_mine(
            &chain.tip_header(),
            chain.tip_state(),
            reward,
            [1u8; 32],
            None,
            ctx.target,
            timestamp.max(ctx.min_timestamp),
        )
        .unwrap()
        .0
    }

    // S1: the halving is enforced by chain.accept itself, on a real chain.
    #[test]
    fn chain_enforces_the_halving_at_the_boundary() {
        // Parameters are a value, so a test can shorten the halving interval
        // and still run the real accept path.
        let p = Params {
            halving_interval: 4,
            ..REGTEST
        };
        let mut chain = Chain::new(p);
        // Space blocks at the target time so difficulty stays at the limit.
        let mut t = 0;
        for _ in 1..p.halving_interval {
            t += p.target_block_time;
            let reward = chain.next_context().reward;
            assert_eq!(reward, p.initial_reward);
            assert!(matches!(chain.accept(mine_from_context(&chain, reward, t)), Accept::Stored { .. }));
        }
        assert_eq!(chain.tip_height(), p.halving_interval - 1);

        // The next block is the first of the halved era.
        let ctx = chain.next_context();
        assert_eq!(ctx.height, p.halving_interval);
        assert_eq!(ctx.reward, p.initial_reward / 2);
        t += p.target_block_time;
        let greedy = mine_from_context(&chain, p.initial_reward, t);
        assert!(matches!(
            chain.accept(greedy),
            Accept::Invalid(BlockError::CoinbaseTooLarge)
        ));
        let honest = mine_from_context(&chain, ctx.reward, t);
        assert!(matches!(chain.accept(honest), Accept::Stored { new_tip: true, .. }));
    }

    // S1: a miner whose clock is behind median-time-past still builds a valid
    // block, because it takes the minimum timestamp from the same context.
    #[test]
    fn block_from_context_is_accepted_with_a_stale_clock() {
        let mut chain = Chain::new(REGTEST);
        for t in [1000, 2000, 3000] {
            let reward = chain.next_context().reward;
            chain.accept(mine_from_context(&chain, reward, t));
        }
        // A clock reading of 5 is far behind median-time-past (2000).
        let reward = chain.next_context().reward;
        let block = mine_from_context(&chain, reward, 5);
        assert!(block.header.timestamp > 2000);
        assert!(matches!(chain.accept(block), Accept::Stored { .. }));
    }

    #[test]
    fn blocks_after_locator_returns_the_gap() {
        let mut chain = Chain::new(REGTEST);
        let mut header = Block::genesis(&REGTEST).header;
        let mut state = State::new();
        for h in 1..=3 {
            let (block, new_state) =
                build_and_mine(&header, &state, REGTEST.initial_reward, [1u8; 32], None, REGTEST.pow_limit, h)
                    .unwrap();
            chain.accept(block.clone());
            header = block.header;
            state = new_state;
        }
        assert_eq!(chain.tip_height(), 3);
        // A fresh node's locator is just genesis.
        let genesis_only = vec![Block::genesis(&REGTEST).hash()];
        let blocks = chain.blocks_after(&genesis_only, 500);
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0].header.height, 1);
    }
}
