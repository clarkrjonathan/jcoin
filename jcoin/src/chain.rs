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

/// One block of history for the difficulty rule: the target it was mined at
/// and its solve time (its timestamp minus its parent's, zero if not later).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sample {
    pub target: U256,
    pub solvetime: u64,
}

/// The target for the next block, from the most recent blocks (newest first).
///
/// This is the LWMA (linearly weighted moving average) rule:
///
/// ```text
/// next = average(targets) * sum(weight_i * solvetime_i) / (N(N+1)/2 * T)
/// ```
///
/// over the last `N = lwma_window` blocks, with weight `N` for the newest
/// block down to `1` for the oldest, and `T = target_block_time`. If the
/// weighted solve time equals `T`, the next target is the average of the
/// recent targets. Faster blocks give a smaller (harder) target, slower
/// blocks a larger one.
///
/// The result is anchored to the **average of the recent targets**, not to
/// the parent's target. A rule that scales the parent's target by a window
/// average applies the same slow-moving ratio again on every block and
/// compounds (the fault in consensus v2).
///
/// - Each solve time counts as at least 1 s and at most
///   `T * solvetime_cap_factor`, so one timestamp has bounded effect.
/// - Where fewer than `N` blocks exist, the missing history counts as blocks
///   at `pow_limit` that took exactly `T`. The rule therefore works from the
///   first block and moves gradually while the window fills.
/// - The result is never easier than `pow_limit` and never zero.
/// - `lwma_window == 0` turns adjustment off: the result is `pow_limit`.
pub fn lwma_next_target(params: &Params, newest_first: &[Sample]) -> [u8; 32] {
    let limit = U256::from_big_endian(&params.pow_limit);
    let n = params.lwma_window;
    let mut bytes = [0u8; 32];
    if n == 0 {
        return params.pow_limit;
    }
    let t = params.target_block_time.max(1);
    let cap = t.saturating_mul(params.solvetime_cap_factor.max(1));
    let padding = Sample {
        target: limit,
        solvetime: t,
    };

    let mut target_sum = U256::zero();
    let mut weighted: u128 = 0;
    for i in 0..n as usize {
        let sample = newest_first.get(i).unwrap_or(&padding);
        // Targets are at most `pow_limit`, well below 2^256 / N, so the sum
        // cannot overflow for any sensible limit; saturate to be safe.
        target_sum = target_sum.saturating_add(sample.target);
        let weight = (n as usize - i) as u128;
        weighted += weight * sample.solvetime.clamp(1, cap) as u128;
    }
    let average = target_sum / U256::from(n);
    let k = (n as u128) * (n as u128 + 1) / 2 * t as u128;
    let (weighted, k) = (U256::from(weighted), U256::from(k));
    // floor(average * weighted / k) without overflowing 256 bits: split
    // `average` into quotient and remainder by `k`. The remainder is below
    // `k`, so `remainder * weighted` is small.
    let (quotient, remainder) = (average / k, average % k);
    let next = quotient
        .saturating_mul(weighted)
        .saturating_add(remainder * weighted / k);
    next.min(limit).max(U256::one()).to_big_endian(&mut bytes);
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

    /// The work of the canonical blocks whose timestamp is `since` or later.
    pub fn work_since(&self, since: u64) -> U256 {
        let mut work = U256::zero();
        let mut cur = &self.blocks[&self.tip];
        while cur.height > 0 && cur.block.header.timestamp >= since {
            work = work.saturating_add(block_work(&cur.block.header.target));
            match self.blocks.get(&cur.block.header.previous_block_hash) {
                Some(parent) => cur = parent,
                None => break,
            }
        }
        work
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
            version: crate::consensus::consensus_version(height),
            reward: self.params.block_reward(height),
            target: self.target_for_child(parent_hash),
            // Strictly greater than median-time-past.
            min_timestamp: self.median_time_past(parent_hash).saturating_add(1),
            parent_timestamp: self.blocks[&parent_hash].block.header.timestamp,
            stall_grace: self.params.stall_grace,
            stall_step: self.params.stall_step,
            pow_limit: self.params.pow_limit,
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
    /// The target a child of `parent_hash` must carry: `lwma_next_target`
    /// over the parent and its ancestors, newest first. Genesis is not a
    /// sample (it has no solve time); its timestamp gives block 1's.
    fn target_for_child(&self, parent_hash: [u8; 32]) -> [u8; 32] {
        let n = self.params.lwma_window as usize;
        let mut samples: Vec<Sample> = Vec::with_capacity(n);
        let mut cur = &self.blocks[&parent_hash];
        while samples.len() < n {
            let Some(parent) = self.blocks.get(&cur.block.header.previous_block_hash) else {
                break; // `cur` is genesis
            };
            samples.push(Sample {
                target: U256::from_big_endian(&cur.block.header.target),
                solvetime: cur
                    .block
                    .header
                    .timestamp
                    .saturating_sub(parent.block.header.timestamp),
            });
            cur = parent;
        }
        lwma_next_target(&self.params, &samples)
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
        let old_tip = self.tip;
        let old_tip_height = self.blocks[&old_tip].height;
        let new_tip = work > self.blocks[&old_tip].work;

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
            // A reorg is a tip switch onto a different branch, abandoning the
            // blocks after the common parent. A plain extension (parent was the
            // old tip) is not a reorg.
            if parent_hash != old_tip {
                let depth = old_tip_height.saturating_sub(height - 1);
                crate::log_info!(
                    [
                        ("event", "reorg".to_string()),
                        ("height", height.to_string()),
                        ("depth", depth.to_string()),
                    ];
                    "[chain] reorg: tip moved to height {height}, {depth} block(s) abandoned"
                );
            }
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

    /// Parameters with difficulty adjustment on, for the rule's own tests
    /// (`REGTEST` has it off). 8 zero bits is trivial to mine.
    fn adjusting(window: u64) -> Params {
        let mut limit = [0xffu8; 32];
        limit[0] = 0;
        Params {
            pow_limit: limit,
            lwma_window: window,
            solvetime_cap_factor: 6,
            target_block_time: 300,
            ..REGTEST
        }
    }

    /// The real network's difficulty settings (window, cap, stall fallback)
    /// on a floor that is trivial to mine and simulate.
    fn mainnet_rule() -> Params {
        Params {
            pow_limit: adjusting(1).pow_limit,
            genesis_timestamp: 0,
            ..crate::consensus::MAINNET
        }
    }

    fn bits(target: U256) -> f64 {
        256.0 - (target.bits() as f64 - 1.0)
            - ((target >> target.bits().saturating_sub(53)).as_u64() as f64).log2()
            + 52.0
    }

    #[test]
    fn lwma_is_steady_on_target_and_bounded_at_the_limits() {
        let p = adjusting(60);
        let limit = U256::from_big_endian(&p.pow_limit);
        let at = |target: U256, solvetime: u64| Sample { target, solvetime };
        let next = |samples: &[Sample]| U256::from_big_endian(&lwma_next_target(&p, samples));

        // No history: the limit. On-target history at one target: that target.
        assert_eq!(next(&[]), limit);
        let hard = limit / 1000;
        assert_eq!(next(&vec![at(hard, 300); 60]), hard);

        // Twice as fast: half the target. Twice as slow: double.
        assert_eq!(next(&vec![at(hard, 150); 60]), hard / 2);
        assert_eq!(next(&vec![at(hard, 600); 60]), hard * 2);

        // Never easier than the limit, never zero, and a solve time counts
        // for at most the cap (6 x 300 s) and at least 1 s.
        assert_eq!(next(&vec![at(limit, 100_000); 60]), limit);
        assert_eq!(next(&vec![at(hard, 100_000); 60]), next(&vec![at(hard, 1800); 60]));
        assert_eq!(next(&vec![at(hard, 0); 60]), next(&vec![at(hard, 1); 60]));
        assert_eq!(next(&vec![at(U256::one(), 1); 60]), U256::one());

        // A window of 0 turns adjustment off.
        assert_eq!(lwma_next_target(&REGTEST, &vec![at(hard, 1); 60]), REGTEST.pow_limit);
    }

    #[test]
    fn one_block_moves_the_difficulty_a_little_and_the_newest_counts_most() {
        let p = adjusting(60);
        let hard = U256::from_big_endian(&p.pow_limit) / 1000;
        let steady = vec![Sample { target: hard, solvetime: 300 }; 60];
        let next = |samples: &[Sample]| U256::from_big_endian(&lwma_next_target(&p, samples));

        // One instant block as the newest of 60: about 2/61 harder, not 4x.
        let mut lucky = steady.clone();
        lucky[0].solvetime = 1;
        let moved = next(&lucky);
        assert!(moved < hard && moved > hard * 96 / 100, "{moved} vs {hard}");

        // The same block as the oldest matters far less.
        let mut old = steady.clone();
        old[59].solvetime = 1;
        assert!(next(&old) > moved);
    }

    /// A deterministic pseudo-random source for the simulations.
    struct Rng(u64);
    impl Rng {
        /// Uniform in (0, 1].
        fn unit(&mut self) -> f64 {
            self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            ((self.0 >> 11) as f64 + 1.0) / (1u64 << 53) as f64
        }
    }

    /// Mine `blocks` blocks under the rule with hash rate `rate(time, height)`
    /// (hashes per second), with exponentially distributed solve times, as
    /// real mining has. Returns each block's (solve time, target).
    /// The stall fallback (`p.stall_grace`, `p.stall_step`) applies as it
    /// does on a real chain: the target a miner works on eases as time passes
    /// without a block.
    fn simulate(
        p: &Params,
        blocks: usize,
        seed: u64,
        rate: impl Fn(u64, usize) -> f64,
    ) -> Vec<(u64, U256)> {
        let easing = (p.stall_grace > 0 && p.stall_step > 0).then_some((p.stall_grace, p.stall_step));
        let limit = U256::from_big_endian(&p.pow_limit);
        let mut rng = Rng(seed);
        let mut samples: std::collections::VecDeque<Sample> = Default::default();
        let (mut time, mut out) = (0u64, Vec::new());
        for height in 1..=blocks {
            let window: Vec<Sample> = samples.iter().copied().collect();
            let base = U256::from_big_endian(&lwma_next_target(p, &window));
            // Expected hashes = 2^256 / target = 2^(leading zero bits).
            let hash_rate = rate(time, height);
            #[allow(unused_assignments)]
            let (mut target, mut solve) = (base, 0f64);
            // Mine through the stretches of constant target until a hit.
            let mut stretch = easing.map(|(grace, _)| grace as f64);
            loop {
                let wait = 2f64.powf(bits(target)) / hash_rate * -rng.unit().ln();
                match stretch {
                    Some(length) if wait > length && target < limit => {
                        solve += length;
                        target = target.saturating_mul(U256::from(2u8)).min(limit);
                        stretch = easing.map(|(_, period)| period as f64);
                    }
                    _ => {
                        solve += wait;
                        break;
                    }
                }
            }
            let solve = solve.round() as u64;
            // What the chain would require of a block with this gap.
            let mut base_bytes = [0u8; 32];
            base.to_big_endian(&mut base_bytes);
            let target = U256::from_big_endian(&crate::consensus::eased_target(
                &base_bytes,
                &p.pow_limit,
                p.stall_grace,
                p.stall_step,
                solve,
            ));
            time += solve;
            samples.push_front(Sample { target, solvetime: solve });
            samples.truncate(p.lwma_window as usize);
            out.push((solve, target));
        }
        out
    }

    fn mean_solve(run: &[(u64, U256)]) -> f64 {
        run.iter().map(|(s, _)| *s as f64).sum::<f64>() / run.len() as f64
    }

    /// The hardest difficulty in `run`, as leading zero bits.
    fn peak_bits(run: &[(u64, U256)]) -> f64 {
        run.iter().map(|(_, t)| bits(*t)).fold(0.0, f64::max)
    }

    // The v2 failure: a miner far faster than the starting difficulty. The
    // rule must find the right difficulty and stay there, on every seed.
    #[test]
    fn a_miner_far_faster_than_the_floor_settles_without_running_away() {
        let p = mainnet_rule();
        // 8-bit floor = 256 hashes; 2^20 hashes/s is 4096 times too fast, as
        // if the first blocks took 0.07 s. Equilibrium: 300 * 2^20 hashes,
        // about 28.2 bits.
        let rate = 2f64.powi(20);
        let equilibrium = (300.0 * rate).log2();
        for seed in 1..=20 {
            let run = simulate(&p, 600, seed, |_, _| rate);
            let settled = mean_solve(&run[200..]);
            assert!((240.0..=375.0).contains(&settled), "seed {seed}: mean {settled}");
            // Never more than 1.5 bits (about 3x) past equilibrium.
            let peak = peak_bits(&run);
            assert!(peak < equilibrium + 1.5, "seed {seed}: peak {peak} vs {equilibrium}");
        }
    }

    #[test]
    fn hash_rate_steps_up_and_down_are_followed() {
        let p = mainnet_rule();
        let base = 2f64.powi(20);
        // 16x more hash power joins at block 500 and leaves at block 900. Each
        // part is measured over 250 blocks, after 150 blocks to settle.
        let rate = move |_, height: usize| if (500..900).contains(&height) { base * 16.0 } else { base };
        for seed in 1..=20 {
            let run = simulate(&p, 1300, seed, rate);
            for (name, part) in [("before", &run[250..500]), ("during", &run[650..900]), ("after", &run[1050..])] {
                let mean = mean_solve(part);
                assert!((225.0..=400.0).contains(&mean), "seed {seed} {name}: mean {mean}");
            }
            // The step up must not overshoot the new equilibrium by much.
            let peak = peak_bits(&run);
            assert!(peak < (300.0 * base * 16.0).log2() + 1.5, "seed {seed}: peak {peak}");
        }
    }

    // Not a pass/fail test: prints how long the chain takes to recover when
    // most hash power leaves, for several settings. Run with
    // `cargo test --release stall_report -- --ignored --nocapture`.
    #[test]
    #[ignore = "a report for choosing parameters, not a check"]
    fn stall_report() {
        let base = 2f64.powi(20);
        println!("hash power falls to 1/drop at block 800; medians over 25 seeds");
        println!("window cap fallback        drop | first block  10 blocks  back to <2T | steady mean  spread   longest gap");
        let settings: [(u64, u64, Option<(u64, u64)>); 8] = [
            (60, 6, None),
            (60, 12, None),
            (30, 6, None),
            (60, 6, Some((1200, 600))),
            (60, 6, Some((1800, 900))),
            (60, 6, Some((3600, 1800))),
            (30, 6, Some((1200, 600))),
            (30, 6, Some((1800, 900))),
        ];
        for (window, cap, easing) in settings {
            for drop in [4.0f64, 15.0, 100.0] {
                let (grace, step) = easing.unwrap_or((0, 0));
                let p = Params {
                    solvetime_cap_factor: cap,
                    stall_grace: grace,
                    stall_step: step,
                    ..adjusting(window)
                };
                let (mut first, mut ten, mut recover, mut means, mut sds, mut gaps) =
                    (vec![], vec![], vec![], vec![], vec![], vec![]);
                for seed in 1..=25 {
                    let rate = |_, h: usize| if h >= 800 { base / drop } else { base };
                    let run = simulate(&p, 1100, seed, rate);
                    let after = &run[799..];
                    first.push(after[0].0 as f64 / 3600.0);
                    ten.push(after[..10].iter().map(|(s, _)| *s as f64).sum::<f64>() / 3600.0);
                    // Hours until a block starts a run of 10 that averages under 2T.
                    let mut elapsed = 0.0;
                    for i in 0..after.len() - 10 {
                        if mean_solve(&after[i..i + 10]) < 600.0 {
                            break;
                        }
                        elapsed += after[i].0 as f64 / 3600.0;
                    }
                    recover.push(elapsed);
                    let steady = &run[400..799];
                    means.push(mean_solve(steady));
                    // Spread of the difficulty, in bits, while hash power is constant.
                    let b: Vec<f64> = steady.iter().map(|(_, t)| bits(*t)).collect();
                    let bm = b.iter().sum::<f64>() / b.len() as f64;
                    sds.push((b.iter().map(|x| (x - bm).powi(2)).sum::<f64>() / b.len() as f64).sqrt());
                    gaps.push(steady.iter().map(|(s, _)| *s).max().unwrap() as f64 / 60.0);
                }
                let median = |v: &mut Vec<f64>| {
                    v.sort_by(|a, b| a.partial_cmp(b).unwrap());
                    v[v.len() / 2]
                };
                let fallback = match easing {
                    Some((g, per)) => format!("x2 at {}m, each {}m", g / 60, per / 60),
                    None => "none".to_string(),
                };
                println!(
                    "{window:6} {cap:3} {fallback:<19} {drop:4} | {:8.1} h {:8.1} h {:10.1} h | {:9.0} s {:5.2} bits {:7.0} min",
                    median(&mut first), median(&mut ten), median(&mut recover),
                    median(&mut means), median(&mut sds), median(&mut gaps)
                );
            }
        }
    }

    // The case that stalled v2 for hours: most hash power leaves. With the
    // stall fallback, blocks keep coming and the chain recovers.
    #[test]
    fn the_chain_keeps_moving_when_hash_power_leaves() {
        let p = mainnet_rule();
        assert!(p.stall_grace > 0, "the real network has the fallback on");
        let base = 2f64.powi(20);
        for drop in [15.0f64, 100.0] {
            let (mut first, mut ten) = (Vec::new(), Vec::new());
            for seed in 1..=20 {
                let run = simulate(&p, 1000, seed, |_, h| if h >= 600 { base / drop } else { base });
                let after = &run[599..];
                first.push(after[0].0 as f64 / 3600.0);
                ten.push(after[..10].iter().map(|(s, _)| *s as f64).sum::<f64>() / 3600.0);
                // And it is back to normal by 250 blocks later.
                let late = mean_solve(&after[250..]);
                assert!((225.0..=400.0).contains(&late), "drop {drop} seed {seed}: mean {late}");
            }
            let worst = |v: &Vec<f64>| v.iter().cloned().fold(0.0, f64::max);
            // No single wait of many hours, on any seed. Without the fallback
            // a 100x drop means about 5 hours to the first block and 2 days
            // to the tenth.
            assert!(worst(&first) < 3.0, "drop {drop}: first block after {} h", worst(&first));
            assert!(worst(&ten) < 12.0, "drop {drop}: ten blocks after {} h", worst(&ten));
        }
    }

    // The fallback on a real chain, through `accept`.
    #[test]
    fn a_late_block_must_carry_the_eased_target_and_an_early_one_may_not() {
        let p = Params { stall_grace: 1200, stall_step: 600, ..adjusting(12) };
        let mut chain = Chain::new(p);
        let mine = |chain: &Chain, target: [u8; 32], time: u64| {
            build_and_mine(&chain.tip_header(), chain.tip_state(), p.initial_reward, [1u8; 32], None, target, time)
                .unwrap()
                .0
        };
        // Fast blocks first, so the base target is harder than the limit and
        // easing has room.
        let mut time = p.genesis_timestamp;
        for _ in 0..12 {
            time += 5;
            let ctx = chain.next_context();
            let block = mine(&chain, ctx.target_at(time), time);
            assert!(matches!(chain.accept(block), Accept::Stored { .. }));
        }
        let ctx = chain.next_context();
        let base = U256::from_big_endian(&ctx.target);
        assert!(base * 8 < U256::from_big_endian(&p.pow_limit));
        assert_eq!(ctx.parent_timestamp, time);

        // 100 s after the parent: only the base target is valid.
        assert_eq!(ctx.target_at(time + 100), ctx.target);
        let eased_once = ctx.target_at(time + 1200);
        assert_eq!(U256::from_big_endian(&eased_once), base * 2);
        let early_claim = mine(&chain, eased_once, time + 100);
        assert!(matches!(chain.accept(early_claim), Accept::Invalid(BlockError::WrongTarget)));

        // 30 minutes after the parent: two doublings are required, and the
        // base target is no longer valid for that timestamp.
        let late = time + 1800;
        assert_eq!(U256::from_big_endian(&ctx.target_at(late)), base * 4);
        let not_eased = mine(&chain, ctx.target, late);
        assert!(matches!(chain.accept(not_eased), Accept::Invalid(BlockError::WrongTarget)));
        let eased = mine(&chain, ctx.target_at(late), late);
        assert!(matches!(chain.accept(eased), Accept::Stored { new_tip: true, .. }));

        // An eased block carries less work than a block at the base target
        // would, so a normal block at the same height wins the fork.
        let normal_parent = chain.tip_header().previous_block_hash;
        let eased_tip = chain.tip_hash();
        let ctx_again = chain.context_for_child(normal_parent);
        let parent = chain.blocks[&normal_parent].block.header.clone();
        let parent_state = chain.blocks[&normal_parent].state.clone();
        let (normal, _) = build_and_mine(&parent, &parent_state, p.initial_reward, [2u8; 32], None, ctx_again.target, time + 300)
            .unwrap();
        assert!(matches!(chain.accept(normal), Accept::Stored { new_tip: true, .. }));
        assert_ne!(chain.tip_hash(), eased_tip);
    }

    // The rule on a real chain: `target_for_child` must feed the same history
    // to `lwma_next_target`, through `accept`, as the simulation does.
    #[test]
    fn a_real_chain_follows_the_rule() {
        let p = adjusting(12);
        let mut chain = Chain::new(p);
        let limit = U256::from_big_endian(&p.pow_limit);
        let mut samples: Vec<Sample> = Vec::new();
        let mut time = p.genesis_timestamp;
        // Solve times well under the 300 s target: difficulty must rise.
        for solve in [300u64, 300, 100, 100, 50, 50, 50, 20, 20, 20, 20, 20, 400, 900, 5000] {
            let expected = lwma_next_target(&p, &samples);
            let ctx = chain.next_context();
            assert_eq!(ctx.target, expected, "at height {}", ctx.height);
            time += solve;
            let (block, _) = build_and_mine(
                &chain.tip_header(),
                chain.tip_state(),
                p.initial_reward,
                [1u8; 32],
                None,
                ctx.target,
                time,
            )
            .unwrap();
            assert!(matches!(chain.accept(block), Accept::Stored { .. }));
            samples.insert(0, Sample { target: U256::from_big_endian(&expected), solvetime: solve });
        }
        assert!(U256::from_big_endian(&chain.next_context().target) < limit);
        // A block mined at the wrong target is refused.
        let (wrong, _) = build_and_mine(
            &chain.tip_header(),
            chain.tip_state(),
            p.initial_reward,
            [1u8; 32],
            None,
            p.pow_limit,
            time + 300,
        )
        .unwrap();
        assert!(matches!(chain.accept(wrong), Accept::Invalid(_)));
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
