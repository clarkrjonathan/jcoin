//! Measure this machine's block-hashing rate and print a POW_LIMIT that
//! should average a chosen block time across a chosen number of racing miners.
//! Run in the same build profile the nodes run in (debug here).

use std::time::Instant;

use primitive_types::U256;

use jcoin::block::Block;

fn main() {
    let miners: f64 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(3.0);
    let seconds: f64 = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(300.0);

    // Hash a header repeatedly, varying the nonce, exactly as mining does.
    let mut header = Block::genesis().header;
    let start = Instant::now();
    let mut hashes: u64 = 0;
    while start.elapsed().as_secs_f64() < 2.0 {
        for _ in 0..50_000 {
            header.nonce = header.nonce.wrapping_add(1);
            let _ = header.block_hash();
            hashes += 1;
        }
    }
    let elapsed = start.elapsed().as_secs_f64();
    let rate = hashes as f64 / elapsed;

    // Expected network hashes per block = miners * rate * block_time.
    let denom = (miners * rate * seconds).round().max(1.0);
    let target = U256::MAX / U256::from(denom as u128);
    let mut bytes = [0u8; 32];
    target.to_big_endian(&mut bytes);

    println!("hashrate: ~{rate:.0} H/s per miner ({hashes} hashes in {elapsed:.2}s)");
    println!("target for {miners} miners, {seconds}s/block: ~{denom:.3e} hashes/block");
    println!("leading-zero bits ~= {:.1}", denom.log2());
    println!("hex: {}", hex::encode(bytes));
    print!("POW_LIMIT = [");
    for (i, b) in bytes.iter().enumerate() {
        if i % 16 == 0 {
            print!("\n    ");
        }
        print!("0x{b:02x}, ");
    }
    println!("\n];");
}
