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

/// Read every block from the log, in append order. A missing file is an empty
/// log. A truncated or corrupt tail stops the read without failing, so a crash
/// mid-append loses only the last record.
pub fn load_blocks(path: &Path) -> io::Result<Vec<Block>> {
    let mut data = Vec::new();
    match File::open(path) {
        Ok(mut f) => {
            f.read_to_end(&mut data)?;
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    }

    let mut out = Vec::new();
    let mut pos = 0usize;
    while pos + 4 <= data.len() {
        let len = u32::from_be_bytes(data[pos..pos + 4].try_into().unwrap()) as usize;
        pos += 4;
        let end = match pos.checked_add(len) {
            Some(e) if e <= data.len() => e,
            _ => break, // truncated tail
        };
        match Block::from_bytes(&data[pos..end]) {
            Some(block) => out.push(block),
            None => break, // corrupt record
        }
        pos = end;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::block::{build_and_mine, Block, BLOCK_REWARD, STATIC_TARGET};
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
        let genesis = Block::genesis();
        let (b1, s1) =
            build_and_mine(&genesis.header, &State::new(), BLOCK_REWARD, [1u8; 32], None, STATIC_TARGET, 1)
                .unwrap();
        let (b2, _) =
            build_and_mine(&b1.header, &s1, BLOCK_REWARD, [1u8; 32], None, STATIC_TARGET, 2).unwrap();

        {
            let mut log = BlockLog::open(&path).unwrap();
            log.append(&b1).unwrap();
            log.append(&b2).unwrap();
        }

        let loaded = load_blocks(&path).unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].hash(), b1.hash());
        assert_eq!(loaded[1].hash(), b2.hash());

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn missing_file_is_empty() {
        let path = temp_path("missing");
        assert!(load_blocks(&path).unwrap().is_empty());
    }
}
