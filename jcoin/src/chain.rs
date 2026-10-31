//! The block store and canonical-chain selection.
//!
//! Blocks are kept by hash, each with its height, cumulative work, and the
//! account state after applying it. The canonical tip is the stored block with
//! the greatest cumulative work (not the greatest height), so competing forks
//! are retained and a reorg is just moving the tip pointer. Fork choice is
//! expressed in terms of work; while the target is static, work is proportional
//! to height, and varying difficulty later changes only `block_work`.

use std::collections::HashMap;

use primitive_types::U256;

use crate::block::{Block, BlockError, BLOCK_REWARD, STATIC_TARGET};
use crate::state::State;

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
    blocks: HashMap<[u8; 32], StoredBlock>,
    tip: [u8; 32],
}

impl Chain {
    /// A new chain holding only the genesis block.
    pub fn new() -> Self {
        let genesis = Block::genesis();
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
        Chain { blocks, tip: hash }
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
        let Some(parent) = self.blocks.get(&parent_hash) else {
            return Accept::Orphan;
        };

        // Difficulty is static for now, so the expected target is the constant.
        let new_state = match block.validate_and_apply(
            &parent.block.header,
            &parent.state,
            BLOCK_REWARD,
            &STATIC_TARGET,
        ) {
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

impl Default for Chain {
    fn default() -> Self {
        Chain::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::{build_and_mine, Block, BLOCK_REWARD, STATIC_TARGET};
    use crate::state::State;

    fn mine_on(parent: &crate::block::BlockHeader, parent_state: &State, miner: [u8; 32]) -> Block {
        build_and_mine(parent, parent_state, BLOCK_REWARD, miner, None, STATIC_TARGET, 1)
            .unwrap()
            .0
    }

    #[test]
    fn accepts_a_chain_and_advances_the_tip() {
        let mut chain = Chain::new();
        let genesis = Block::genesis();
        let b1 = mine_on(&genesis.header, &State::new(), [1u8; 32]);
        match chain.accept(b1.clone()) {
            Accept::Stored { new_tip: true, height: 1 } => {}
            _ => panic!("block 1 should extend the tip"),
        }
        assert_eq!(chain.tip_height(), 1);
        assert_eq!(chain.tip_state().balance(&[1u8; 32]), BLOCK_REWARD);
    }

    #[test]
    fn unknown_parent_is_orphan() {
        let mut chain = Chain::new();
        // Mine on a fake parent the chain does not have.
        let mut fake_parent = Block::genesis().header;
        fake_parent.state_root = [9u8; 32];
        let orphan = mine_on(&fake_parent, &State::new(), [1u8; 32]);
        assert!(matches!(chain.accept(orphan), Accept::Orphan));
    }

    /// Build `len` blocks on top of genesis, all crediting `miner`.
    fn branch(len: u64, miner: [u8; 32]) -> Vec<Block> {
        let mut header = Block::genesis().header;
        let mut state = State::new();
        let mut out = Vec::new();
        for _ in 0..len {
            let (block, new_state) =
                build_and_mine(&header, &state, BLOCK_REWARD, miner, None, STATIC_TARGET, 1)
                    .unwrap();
            header = block.header.clone();
            state = new_state;
            out.push(block);
        }
        out
    }

    #[test]
    fn longer_branch_wins_by_work() {
        let mut chain = Chain::new();
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
        assert_eq!(chain.tip_state().balance(&[2u8; 32]), 3 * BLOCK_REWARD);
        assert_eq!(chain.tip_state().balance(&[1u8; 32]), 0);
    }

    #[test]
    fn blocks_after_locator_returns_the_gap() {
        let mut chain = Chain::new();
        let mut header = Block::genesis().header;
        let mut state = State::new();
        for _ in 0..3 {
            let (block, new_state) =
                build_and_mine(&header, &state, BLOCK_REWARD, [1u8; 32], None, STATIC_TARGET, 1)
                    .unwrap();
            chain.accept(block.clone());
            header = block.header;
            state = new_state;
        }
        assert_eq!(chain.tip_height(), 3);
        // A fresh node's locator is just genesis.
        let genesis_only = vec![Block::genesis().hash()];
        let blocks = chain.blocks_after(&genesis_only, 500);
        assert_eq!(blocks.len(), 3);
        assert_eq!(blocks[0].header.height, 1);
    }
}
