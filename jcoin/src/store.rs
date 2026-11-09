//! On-disk persistence for the chain: an append-only log of blocks.
//!
//! Only blocks are stored; account state is never persisted, it is rebuilt by
//! replaying the log on startup. Blocks are the single source of truth. The log
//! is a flat file of records, each `length (u32, big-endian) | block bytes`.
//! A block is appended only after it has been accepted, so a parent is always
//! written before its child and the file replays in order.

use std::fs::{File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::Path;

use crate::block::Block;

/// Append-only handle to the block log.
pub struct BlockLog {
    file: File,
}

impl BlockLog {
    /// Open (creating if needed) the log for appending.
    pub fn open(path: &Path) -> io::Result<BlockLog> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)?;
        Ok(BlockLog { file })
    }

    /// Append one block and flush it.
    pub fn append(&mut self, block: &Block) -> io::Result<()> {
        let bytes = block.serialize();
        self.file.write_all(&(bytes.len() as u32).to_be_bytes())?;
        self.file.write_all(&bytes)?;
        self.file.flush()?;
        Ok(())
    }
}

/// The readable prefix of a block log.
pub struct LoadedLog {
    /// Every block before the first torn or corrupt record, in append order.
    pub blocks: Vec<Block>,
    /// Byte offset of the end of the last good record.
    pub good_len: u64,
    /// Bytes after `good_len` (a torn or corrupt tail). Zero for a clean log.
    pub discarded: u64,
}

/// Read every block from the log, in append order. A missing file is an empty
/// log. A truncated or corrupt record stops the read without failing; the
/// bytes from that record on are reported as `discarded`.
pub fn load_blocks(path: &Path) -> io::Result<LoadedLog> {
    let mut data = Vec::new();
    match File::open(path) {
        Ok(mut f) => {
            f.read_to_end(&mut data)?;
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Ok(LoadedLog {
                blocks: Vec::new(),
                good_len: 0,
                discarded: 0,
            })
        }
        Err(e) => return Err(e),
    }

    let mut blocks = Vec::new();
    let mut pos = 0usize;
    while pos + 4 <= data.len() {
        let len = u32::from_be_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
        let start = pos + 4;
        let end = match start.checked_add(len) {
            Some(e) if e <= data.len() => e,
            _ => break, // truncated tail
        };
        match Block::from_bytes(&data[start..end]) {
            Some(block) => blocks.push(block),
            None => break, // corrupt record
        }
        pos = end;
    }
    Ok(LoadedLog {
        blocks,
        good_len: pos as u64,
        discarded: (data.len() - pos) as u64,
    })
}

/// Load the log and cut off any torn or corrupt tail, so that later appends
/// follow the last good record. Without the cut, a block appended after a torn
/// record would be unreadable on every later restart.
pub fn load_and_repair(path: &Path) -> io::Result<LoadedLog> {
    let loaded = load_blocks(path)?;
    if loaded.discarded > 0 {
        let file = OpenOptions::new().write(true).open(path)?;
        file.set_len(loaded.good_len)?;
        file.sync_all()?;
    }
    Ok(loaded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::{build_and_mine, Block};
    use crate::consensus::REGTEST;
    use crate::state::State;

    fn temp_path(name: &str) -> std::path::PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!("jcoin-store-test-{}-{}.dat", std::process::id(), name));
        let _ = std::fs::remove_file(&p);
        p
    }

    #[test]
    fn append_and_load_round_trip() {
        let path = temp_path("roundtrip");
        let genesis = Block::genesis(&REGTEST);
        let (b1, s1) =
            build_and_mine(&genesis.header, &State::new(), REGTEST.initial_reward, [1u8; 32], None, REGTEST.pow_limit, 1)
                .unwrap();
        let (b2, _) =
            build_and_mine(&b1.header, &s1, REGTEST.initial_reward, [1u8; 32], None, REGTEST.pow_limit, 2).unwrap();

        {
            let mut log = BlockLog::open(&path).unwrap();
            log.append(&b1).unwrap();
            log.append(&b2).unwrap();
        }

        let loaded = load_blocks(&path).unwrap().blocks;
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].hash(), b1.hash());
        assert_eq!(loaded[1].hash(), b2.hash());

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn missing_file_is_empty() {
        let path = temp_path("missing");
        assert!(load_blocks(&path).unwrap().blocks.is_empty());
    }

    // A crash mid-append leaves a torn record. After repair, a block appended
    // later must still load (before the fix it was hidden behind the torn record).
    #[test]
    fn torn_tail_is_cut_so_later_appends_survive() {
        let path = temp_path("torn");
        let genesis = Block::genesis(&REGTEST);
        let (b1, s1) =
            build_and_mine(&genesis.header, &State::new(), REGTEST.initial_reward, [1u8; 32], None, REGTEST.pow_limit, 1)
                .unwrap();
        let (b2, _) =
            build_and_mine(&b1.header, &s1, REGTEST.initial_reward, [1u8; 32], None, REGTEST.pow_limit, 2).unwrap();

        {
            let mut log = BlockLog::open(&path).unwrap();
            log.append(&b1).unwrap();
        }
        let clean_len = std::fs::metadata(&path).unwrap().len();
        // Simulate a torn write: a length prefix and half a block.
        {
            let mut f = OpenOptions::new().append(true).open(&path).unwrap();
            let bytes = b2.serialize();
            f.write_all(&(bytes.len() as u32).to_be_bytes()).unwrap();
            f.write_all(&bytes[..bytes.len() / 2]).unwrap();
        }

        let repaired = load_and_repair(&path).unwrap();
        assert_eq!(repaired.blocks.len(), 1);
        assert_eq!(repaired.good_len, clean_len);
        assert!(repaired.discarded > 0);
        assert_eq!(std::fs::metadata(&path).unwrap().len(), clean_len);

        // Append after repair, then reload: both blocks are there.
        {
            let mut log = BlockLog::open(&path).unwrap();
            log.append(&b2).unwrap();
        }
        let reloaded = load_blocks(&path).unwrap();
        assert_eq!(reloaded.discarded, 0);
        assert_eq!(reloaded.blocks.len(), 2);
        assert_eq!(reloaded.blocks[1].hash(), b2.hash());

        std::fs::remove_file(&path).ok();
    }
}
