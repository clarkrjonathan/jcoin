//! Consensus parameters and the per-block rule context.
//!
//! `Params` holds every tunable consensus value as data. A node runs with one
//! parameter set: `MAINNET` (the real network) or `REGTEST` (easy proof of work
//! and a zero genesis timestamp, for tests and fast local runs). Tests use the
//! same code paths as the real network with different values, instead of
//! `cfg(test)` constants that silently swap the rules.
//!
//! `BlockContext` is what consensus requires of one specific next block: its
//! height, the subsidy it may claim, the target it must use, and its minimum
//! timestamp. `Chain` computes it once from the parent's ancestry; validation
//! checks a block against it, and the miner builds from the same value. No
//! caller derives a consensus input on its own.

use crate::amount::COIN;

/// Consensus parameters. Two nodes agree on blocks only if they use the same
/// parameter set (the genesis block is derived from it).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Params {
    /// The easiest allowed target, and the genesis / first-epoch target.
    /// Difficulty retargets harder than this as needed but never easier.
    pub pow_limit: [u8; 32],
    /// Genesis block timestamp (Unix seconds).
    pub genesis_timestamp: u64,
    /// Target average seconds between blocks. Difficulty retargets toward it.
    pub target_block_time: u64,
    /// Number of recent blocks in the LWMA difficulty window. Difficulty is
    /// recomputed every block from a linearly weighted average of the solvetimes
    /// of the last `lwma_window` blocks (recent blocks weighted more).
    pub lwma_window: u64,
    /// Largest per-step difficulty change factor (clamps each retarget).
    pub max_adjust_factor: u64,
    /// Number of recent blocks whose median timestamp a new block must exceed.
    pub median_time_span: usize,
    /// Block subsidy at height 1, before any halving (base units).
    pub initial_reward: u64,
    /// Blocks between subsidy halvings.
    pub halving_interval: u64,
}

/// The real network (consensus v2).
///
/// `pow_limit` (top 28 bits zero) is the easy starting point and genesis target.
/// It is sized for the weakest launch miner mining alone: the reference node's
/// CPU (~1 MH/s) at 300 s/block needs ~28 leading-zero bits. Stronger miners
/// (GPUs measure ~100 MH/s, ~35 bits) are absorbed by the LWMA retarget when
/// they join, so this only has to be a sane floor (ADR 0001).
/// `genesis_timestamp` is pinned to the launch time (the v2 genesis cut,
/// 2026-11-11 00:52 UTC). Every node must build from this exact value or it
/// derives a different genesis block and rejects the shared chain.
/// `halving_interval` = 32,000 blocks, about 111 days at 5-minute blocks, puts
/// the 33rd halving (where the subsidy reaches zero) at about 10 years. Total
/// supply is just under `2 * initial_reward * halving_interval` (3,200,000
/// JEF), about 89% of it minted in year 1.
pub const MAINNET: Params = Params {
    pow_limit: limit_with_zero_bits(28),
    genesis_timestamp: 1_794_358_345, // 2026-11-11 00:52 UTC, the v2 genesis cut
    target_block_time: 300,
    lwma_window: 60,
    max_adjust_factor: 4,
    median_time_span: 11,
    initial_reward: 50 * COIN,
    halving_interval: 32_000,
};

/// A local test network: the same rules as `MAINNET` with an easy proof of
/// work (top 16 bits zero, so blocks mine in milliseconds), a zero genesis
/// timestamp (so tests can use small increasing timestamps), a short
/// `target_block_time` (so a local run settles to a watchable block cadence
/// instead of ramping up to the 5-minute mainnet target), and a short halving
/// interval (so a test can cross a halving on a real chain).
pub const REGTEST: Params = Params {
    pow_limit: limit_with_zero_bits(16),
    genesis_timestamp: 0,
    target_block_time: 10,
    lwma_window: 12,
    max_adjust_factor: 4,
    median_time_span: 11,
    initial_reward: 50 * COIN,
    halving_interval: 150,
};

/// The consensus version a block at `height` must carry. A future rule change
/// bumps this at a chosen activation height, so nodes validate by the version the
/// height selects and no restart is needed. v2 is the current (genesis) rule set.
pub const fn consensus_version(_height: u64) -> u32 {
    2
}

/// A target whose top `bits` bits are zero and the rest one.
const fn limit_with_zero_bits(bits: usize) -> [u8; 32] {
    let mut out = [0xffu8; 32];
    let mut i = 0;
    while i < 32 {
        let bit = i * 8;
        if bit + 8 <= bits {
            out[i] = 0;
        } else if bit < bits {
            out[i] = 0xff >> (bits - bit);
        }
        i += 1;
    }
    out
}

impl Params {
    /// The block subsidy (excluding fees) a coinbase at `height` may claim:
    /// `initial_reward >> (height / halving_interval)`. After that reaches zero,
    /// miners are paid by fees only.
    pub fn block_reward(&self, height: u64) -> u64 {
        let halvings = height / self.halving_interval;
        // A shift by 64 or more is undefined for u64, and the result is 0 anyway.
        if halvings >= 64 {
            0
        } else {
            self.initial_reward >> halvings
        }
    }
}

/// What consensus requires of the next block on a given parent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockContext {
    /// The block's height (parent height + 1).
    pub height: u64,
    /// The consensus version the header must carry (`consensus_version(height)`).
    pub version: u32,
    /// The largest subsidy the coinbase may claim, excluding fees.
    pub reward: u64,
    /// The exact target the header must carry.
    pub target: [u8; 32],
    /// The smallest allowed timestamp (median-time-past + 1).
    pub min_timestamp: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_have_the_stated_zero_bits() {
        let main = MAINNET.pow_limit;
        // 28 leading zero bits: bytes 0..3 zero, byte 3 = 0xff >> 4 = 0x0f.
        assert_eq!(&main[..4], &[0x00, 0x00, 0x00, 0x0f]);
        assert!(main[4..].iter().all(|&b| b == 0xff));
        let reg = REGTEST.pow_limit;
        assert_eq!(&reg[..3], &[0x00, 0x00, 0xff]);
        assert_eq!(limit_with_zero_bits(0), [0xffu8; 32]);
    }

    #[test]
    fn reward_halves_every_interval() {
        let p = MAINNET;
        let r = p.initial_reward;
        assert_eq!(r, 50 * COIN);
        assert_eq!(p.block_reward(1), r);
        assert_eq!(p.block_reward(p.halving_interval - 1), r);
        assert_eq!(p.block_reward(p.halving_interval), r / 2);
        assert_eq!(p.block_reward(2 * p.halving_interval - 1), r / 2);
        assert_eq!(p.block_reward(2 * p.halving_interval), r / 4);
        // 50 JEF = 5e9 base units: the 32nd halving is the last nonzero reward.
        assert_eq!(p.block_reward(32 * p.halving_interval), 1);
        assert_eq!(p.block_reward(33 * p.halving_interval), 0);
        // No shift overflow at extreme heights.
        assert_eq!(p.block_reward(64 * p.halving_interval), 0);
        assert_eq!(p.block_reward(u64::MAX), 0);
    }

    #[test]
    fn total_supply_is_bounded() {
        let p = MAINNET;
        // Height 0 is genesis (no coinbase), so start at height 1.
        let mut supply: u64 = 0;
        for era in 0..64 {
            let first = (era * p.halving_interval).max(1);
            let blocks = (era + 1) * p.halving_interval - first;
            supply += blocks * p.block_reward(era * p.halving_interval);
        }
        assert!(supply < 3_200_000 * COIN);
        assert!(supply > 3_199_000 * COIN);
    }
}
