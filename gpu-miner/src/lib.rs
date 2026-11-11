//! JeffyCoin GPU miner as a library: the GPU kernel (`gpu`), the proof-of-work
//! helpers (`pow`), and the mining loop (`miner`). The `gpu-miner` binary is a
//! command-line front end; the desktop app embeds the same loop.

pub mod gpu;
pub mod miner;
pub mod pow;
