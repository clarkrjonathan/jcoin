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

use primitive_types::U256;

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
    /// Number of recent blocks in the LWMA difficulty window (see
    /// `chain::lwma_next_target`). `0` turns adjustment off: every block uses
    /// `pow_limit`.
    pub lwma_window: u64,
    /// The longest solve time one block counts for in the difficulty average,
    /// as a multiple of `target_block_time`. It bounds how much one timestamp
    /// can move the difficulty, and sets how fast difficulty falls after hash
    /// power leaves.
    pub solvetime_cap_factor: u64,
    /// Stall fallback: once a block's timestamp is this many seconds after its
    /// parent's, its target is doubled (see `eased_target`). `0` turns the
    /// fallback off.
    pub stall_grace: u64,
    /// Stall fallback: after `stall_grace`, the target doubles again every
    /// this many seconds.
    pub stall_step: u64,
    /// Number of recent blocks whose median timestamp a new block must exceed.
    pub median_time_span: usize,
    /// Block subsidy at height 1, before any halving (base units).
    pub initial_reward: u64,
    /// Blocks between subsidy halvings.
    pub halving_interval: u64,
}

/// The real network (consensus v3, the chain restarted in release 2.2.0).
///
/// `pow_limit` (top 28 bits zero) is the easiest difficulty: the starting
/// point, and the floor the stall fallback can ease down to. It is about 300 s
/// of work for the weakest node mining alone (a CPU at ~1 MH/s). The
/// difficulty rule adjusts from the first block (`chain::lwma_next_target`),
/// so GPUs far faster than this are absorbed within the first window.
/// `lwma_window` 30, `solvetime_cap_factor` 6, and the stall fallback (target
/// doubles 20 minutes after the parent, then every 10 minutes) were chosen
/// from chain simulations; see `CHANGELOG.md`, 2.2.0.
/// `genesis_timestamp` is pinned to the restart (2026-11-11 19:05 UTC).
/// Every node must build from this exact value or it
/// derives a different genesis block and rejects the shared chain.
/// `halving_interval` = 32,000 blocks, about 111 days at 5-minute blocks, puts
/// the 33rd halving (where the subsidy reaches zero) at about 10 years. Total
/// supply is just under `2 * initial_reward * halving_interval` (3,200,000
/// JEF), about 89% of it minted in year 1.
pub const MAINNET: Params = Params {
    pow_limit: limit_with_zero_bits(28),
    genesis_timestamp: 1_794_423_946, // 2026-11-11 19:05 UTC, the v3 genesis (release 2.2.0)
    target_block_time: 300,
    lwma_window: 30,
    solvetime_cap_factor: 6,
    stall_grace: 20 * 60,
    stall_step: 10 * 60,
    median_time_span: 11,
    initial_reward: 50 * COIN,
    halving_interval: 32_000,
};

/// A local test network: the same rules as `MAINNET` with an easy proof of
/// work (top 16 bits zero, so blocks mine in milliseconds), a zero genesis
/// timestamp (so tests can use small increasing timestamps), no difficulty
/// adjustment (`lwma_window` 0, so a test can mine every block at `pow_limit`
/// with any timestamps), a short `target_block_time` (so a local run settles to a watchable block cadence
/// instead of ramping up to the 5-minute mainnet target), and a short halving
/// interval (so a test can cross a halving on a real chain).
pub const REGTEST: Params = Params {
    pow_limit: limit_with_zero_bits(16),
    genesis_timestamp: 0,
    target_block_time: 10,
    lwma_window: 0,
    solvetime_cap_factor: 6,
    stall_grace: 0,
    stall_step: 0,
    median_time_span: 11,
    initial_reward: 50 * COIN,
    halving_interval: 150,
};

/// The newest consensus version this build knows. The version rises with
/// every consensus change, also when the chain restarts, and is never reused:
/// other nodes read a higher version in a block as "newer rules exist" (see
/// `signal`). Version 1 was the first demo chain, version 2 the chain of
/// releases 2.0.0 to 2.1.2 (its difficulty rule compounded), version 3 the
/// restarted chain of 2.2.0.
pub const LATEST_VERSION: u32 = 3;

/// The consensus version a block at `height` must carry. A future rule change
/// raises this at a chosen activation height, so nodes validate by the version
/// the height selects and no restart is needed.
pub const fn consensus_version(_height: u64) -> u32 {
    LATEST_VERSION
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
    /// The target the header must carry if its timestamp is less than
    /// `stall_grace` after the parent's. A later block may carry an easier
    /// one: use `target_at` for the exact requirement.
    pub target: [u8; 32],
    /// The parent block's timestamp.
    pub parent_timestamp: u64,
    /// `Params::stall_grace`, `Params::stall_step`, and `Params::pow_limit`,
    /// carried here so `target_at` needs nothing else.
    pub stall_grace: u64,
    pub stall_step: u64,
    pub pow_limit: [u8; 32],
    /// The smallest allowed timestamp (median-time-past + 1).
    pub min_timestamp: u64,
}

impl BlockContext {
    /// The exact target a block with this `timestamp` must carry.
    pub fn target_at(&self, timestamp: u64) -> [u8; 32] {
        eased_target(
            &self.target,
            &self.pow_limit,
            self.stall_grace,
            self.stall_step,
            timestamp.saturating_sub(self.parent_timestamp),
        )
    }
}

/// The stall fallback. `base` is the target the difficulty rule gives. A
/// block whose timestamp is `gap` seconds after its parent's must carry:
///
/// - `base`, while `gap < grace`;
/// - `base` doubled once at `gap == grace`, and once more for every `step`
///   seconds after that, never easier than `limit`.
///
/// So when hash power leaves and no block is found, the next block becomes
/// easier as time passes, and the chain cannot stall for long. With
/// `grace == 0` (or `step == 0`) the fallback is off.
///
/// The gap comes from the block's own timestamp. A node accepts a block only
/// if that timestamp is at most `block::MAX_FUTURE_DRIFT` ahead of its clock,
/// and that limit is shorter than `grace` on the real network, so a miner
/// cannot claim easing for time that has not passed. An eased block also
/// carries less work, so a block at `base` at the same height wins.
pub fn eased_target(base: &[u8; 32], limit: &[u8; 32], grace: u64, step: u64, gap: u64) -> [u8; 32] {
    if grace == 0 || step == 0 || gap < grace {
        return *base;
    }
    let doublings = 1 + (gap - grace) / step;
    let (base, limit) = (U256::from_big_endian(base), U256::from_big_endian(limit));
    // Past 255 doublings any target is at the limit.
    let eased = if doublings > 255 || base.leading_zeros() < doublings as u32 {
        limit
    } else {
        (base << doublings as usize).min(limit)
    };
    let mut bytes = [0u8; 32];
    eased.to_big_endian(&mut bytes);
    bytes
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
    fn the_stall_fallback_doubles_the_target_as_time_passes() {
        let limit = [0x0fu8; 32];
        let mut base = [0u8; 32];
        base[4] = 0x10; // far harder than the limit
        let at = |gap| U256::from_big_endian(&eased_target(&base, &limit, 1200, 600, gap));
        let b = U256::from_big_endian(&base);
        assert_eq!(at(0), b);
        assert_eq!(at(1199), b);
        assert_eq!(at(1200), b * 2);
        assert_eq!(at(1799), b * 2);
        assert_eq!(at(1800), b * 4);
        assert_eq!(at(1200 + 600 * 9), b * 1024);
        // Never easier than the limit, however long the gap.
        assert_eq!(at(1200 + 600 * 40), U256::from_big_endian(&limit));
        assert_eq!(at(u64::MAX), U256::from_big_endian(&limit));
        // Off when either setting is zero.
        assert_eq!(eased_target(&base, &limit, 0, 600, 99_999), base);
        assert_eq!(eased_target(&base, &limit, 1200, 0, 99_999), base);

        let ctx = BlockContext {
            height: 5,
            version: 3,
            reward: 0,
            target: base,
            min_timestamp: 0,
            parent_timestamp: 10_000,
            stall_grace: 1200,
            stall_step: 600,
            pow_limit: limit,
        };
        assert_eq!(ctx.target_at(10_500), base);
        assert_eq!(U256::from_big_endian(&ctx.target_at(11_200)), b * 2);
        assert_eq!(ctx.target_at(9_000), base, "a timestamp before the parent's claims nothing");
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
