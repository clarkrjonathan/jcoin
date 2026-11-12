//! JeffyCoin GPU miner as a library: the GPU search (`gpu`), its shader
//! variants (`kernel`), the proof-of-work helpers (`pow`), and the mining
//! loop (`miner`). The `gpu-miner` binary is a
//! command-line front end; the desktop app embeds the same loop.

pub mod gpu;
pub mod kernel;
pub mod miner;
pub mod pow;
