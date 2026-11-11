//! Evidence that the network follows newer rules than this node knows.
//!
//! The block version rises on every consensus change and is never reused. A
//! block this node refuses because its version is higher than
//! `consensus::LATEST_VERSION`, but whose header meets the target the header
//! states, is proof of work spent on newer rules. The node can check that
//! without knowing the newer rules, for a fork of its own chain and for a
//! chain with another genesis.
//!
//! The node adds up that work over the last 24 hours and compares it with the
//! work on its own chain in the same period. The result is advice for the
//! person running the node: nothing is refused or stopped because of it.

use std::collections::{HashSet, VecDeque};

use primitive_types::U256;

use crate::block::BlockHeader;
use crate::chain::block_work;
use crate::consensus::{Params, LATEST_VERSION};

/// How far back evidence counts, in seconds.
pub const WINDOW: u64 = 24 * 60 * 60;
/// Most blocks remembered. Older ones are forgotten first.
const CAPACITY: usize = 4096;
/// Newer-version blocks needed, besides the work, for `Level::Strong`.
const STRONG_MIN_BLOCKS: usize = 3;

/// How strong the evidence is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// No block with a newer version was seen in the window.
    None,
    /// Some were seen: an update may exist.
    Notice,
    /// They carry at least as much work as this node's own chain did in the
    /// window: most of the network's work follows rules this node cannot read.
    Strong,
}

impl Level {
    pub fn as_str(&self) -> &'static str {
        match self {
            Level::None => "none",
            Level::Notice => "notice",
            Level::Strong => "strong",
        }
    }
}

/// A summary of the evidence, for the REST endpoint and the app.
#[derive(Debug, Clone, PartialEq)]
pub struct Summary {
    pub level: Level,
    /// The highest block version seen in the window (0 if none).
    pub newest_version: u32,
    /// Newer-version blocks seen in the window.
    pub blocks: usize,
    /// Their total work divided by the work on this node's own chain in the
    /// window. Large if this node's chain found nothing in the window.
    pub work_ratio: f64,
}

/// Newer-version blocks seen, by time of arrival.
#[derive(Default)]
pub struct Signals {
    seen: HashSet<[u8; 32]>,
    /// (arrival time, block hash, work, version), oldest first.
    entries: VecDeque<(u64, [u8; 32], U256, u32)>,
}

impl Signals {
    /// Look at a block header that arrived at `now` (Unix seconds). Returns
    /// true if it is new evidence. A header counts only if its version is
    /// newer than this node knows, its hash meets its own target, and that
    /// target is no easier than this network's limit (so it cost at least as
    /// much as the cheapest block here).
    pub fn observe(&mut self, params: &Params, header: &BlockHeader, now: u64) -> bool {
        if header.version <= LATEST_VERSION {
            return false;
        }
        let target = U256::from_big_endian(&header.target);
        if target > U256::from_big_endian(&params.pow_limit) || !header.meets_target() {
            return false;
        }
        let hash = header.block_hash();
        if !self.seen.insert(hash) {
            return false;
        }
        self.entries.push_back((now, hash, block_work(&header.target), header.version));
        while self.entries.len() > CAPACITY {
            if let Some((_, old, _, _)) = self.entries.pop_front() {
                self.seen.remove(&old);
            }
        }
        true
    }

    /// The evidence in the window ending at `now`, against `own_work`: the
    /// work on this node's own chain in the same window.
    pub fn summary(&mut self, now: u64, own_work: U256) -> Summary {
        while self.entries.front().is_some_and(|(t, ..)| t.saturating_add(WINDOW) < now) {
            if let Some((_, old, _, _)) = self.entries.pop_front() {
                self.seen.remove(&old);
            }
        }
        let blocks = self.entries.len();
        let work = self
            .entries
            .iter()
            .fold(U256::zero(), |sum, (_, _, w, _)| sum.saturating_add(*w));
        let newest_version = self.entries.iter().map(|(.., v)| *v).max().unwrap_or(0);
        let level = if blocks == 0 {
            Level::None
        } else if blocks >= STRONG_MIN_BLOCKS && work >= own_work {
            Level::Strong
        } else {
            Level::Notice
        };
        Summary {
            level,
            newest_version,
            blocks,
            work_ratio: ratio(work, own_work),
        }
    }
}

/// `a / b` as a float, for display. 1e9 if `b` is zero and `a` is not.
fn ratio(a: U256, b: U256) -> f64 {
    if a.is_zero() {
        return 0.0;
    }
    if b.is_zero() {
        return 1e9;
    }
    // Scale both into 64 bits by the same shift, then divide.
    let shift = a.bits().max(b.bits()).saturating_sub(63);
    let (a, b) = ((a >> shift).as_u64() as f64, (b >> shift).as_u64().max(1) as f64);
    (a / b).min(1e9)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::mine_chunk;
    use crate::consensus::REGTEST;

    /// A header with the given version that meets `target`.
    fn mined(version: u32, target: [u8; 32], salt: u64) -> BlockHeader {
        let mut header = BlockHeader {
            version,
            height: 7,
            previous_block_hash: [9u8; 32],
            transactions_hash: [0u8; 32],
            state_root: [0u8; 32],
            timestamp: salt,
            target,
            nonce: 0,
        };
        while !mine_chunk(&mut header, 1 << 20) {}
        header
    }

    #[test]
    fn only_newer_versions_with_real_work_count() {
        let p = REGTEST;
        let mut signals = Signals::default();
        // The version this node knows is not evidence.
        assert!(!signals.observe(&p, &mined(LATEST_VERSION, p.pow_limit, 1), 100));
        // A newer version without proof of work is not evidence.
        let mut unmined = mined(LATEST_VERSION + 1, p.pow_limit, 2);
        unmined.target = [0u8; 32];
        assert!(!signals.observe(&p, &unmined, 100));
        // Nor is one mined at a target easier than this network allows.
        assert!(!signals.observe(&p, &mined(LATEST_VERSION + 1, [0xff; 32], 3), 100));
        assert_eq!(signals.summary(100, U256::zero()).level, Level::None);

        // A mined newer-version block is, once.
        let block = mined(LATEST_VERSION + 1, p.pow_limit, 4);
        assert!(signals.observe(&p, &block, 100));
        assert!(!signals.observe(&p, &block, 101));
        let s = signals.summary(101, U256::zero());
        assert_eq!((s.level, s.blocks, s.newest_version), (Level::Notice, 1, LATEST_VERSION + 1));
    }

    #[test]
    fn the_level_follows_the_work_and_evidence_expires() {
        let p = REGTEST;
        let one = block_work(&p.pow_limit);
        let mut signals = Signals::default();
        for i in 0..4 {
            assert!(signals.observe(&p, &mined(LATEST_VERSION + 2, p.pow_limit, 10 + i), 1000));
        }
        // Four blocks of newer work against ten of our own: a notice.
        let s = signals.summary(1000, one * 10);
        assert_eq!(s.level, Level::Notice);
        assert!((s.work_ratio - 0.4).abs() < 0.01, "{}", s.work_ratio);
        // Against three of our own: more work follows the newer rules.
        assert_eq!(signals.summary(1000, one * 3).level, Level::Strong);
        // Our chain found nothing in the window at all.
        assert_eq!(signals.summary(1000, U256::zero()).level, Level::Strong);
        // After the window the evidence is gone.
        assert_eq!(signals.summary(1000 + WINDOW + 1, one).level, Level::None);
        assert_eq!(ratio(U256::zero(), one), 0.0);
    }
}
