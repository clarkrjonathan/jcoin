//! JeffyCoin GPU miner.
//!
//! Gets block templates from a node (GET /mining/work/{miner}), searches the
//! nonce on the GPU, checks every hit on the CPU, and submits it
//! (POST /mining/submit). The GPU kernel is WGSL run through wgpu, so it works
//! on NVIDIA, AMD, and Intel GPUs over Vulkan, DirectX 12, or Metal.
//!
//!   gpu-miner --list                         GPUs wgpu can use
//!   gpu-miner --self-test                    GPU results equal CPU SHA-256
//!   gpu-miner --bench 10                     hash rate, no node needed
//!   gpu-miner --miner <hex address>          mine against http://127.0.0.1:3000

mod gpu;
mod pow;

use std::time::{Duration, Instant};

use clap::Parser;
use rand::Rng;
use serde::Deserialize;

use gpu::{Gpu, WORKGROUP};
use pow::{gpu_work, meets_target, HEADER_LEN};

#[derive(Parser)]
#[command(name = "gpu-miner", about = "JeffyCoin GPU miner")]
struct Cli {
    /// Node REST address.
    #[arg(long, default_value = "http://127.0.0.1:3000")]
    node: String,
    /// Address (hex public key, 32 bytes) that receives the block rewards.
    #[arg(long)]
    miner: Option<String>,
    /// GPU to use, by index from --list. Default: the high-performance one.
    #[arg(long)]
    adapter: Option<usize>,
    /// Nonces per GPU dispatch (rounded to a multiple of 256). Larger batches
    /// waste less time on overhead but react later to a new block.
    #[arg(long, default_value_t = 1 << 22)]
    batch: u32,
    /// Fetch a fresh template at least this often (seconds), to pick up new
    /// transactions and a current timestamp.
    #[arg(long, default_value_t = 20)]
    refresh: u64,
    /// List the GPUs and exit.
    #[arg(long)]
    list: bool,
    /// Compare GPU results with the CPU on random headers and exit.
    #[arg(long)]
    self_test: bool,
    /// Measure the hash rate for this many seconds and exit.
    #[arg(long)]
    bench: Option<u64>,
}

#[derive(Deserialize)]
struct Work {
    job: u64,
    height: u64,
    header: String,
    target: String,
}

#[derive(Deserialize)]
struct ChainInfo {
    height: u64,
}

fn main() {
    let cli = Cli::parse();
    if cli.list {
        for (i, a) in gpu::adapters().iter().enumerate() {
            println!("{i}: {}", gpu::describe(a));
        }
        return;
    }
    let gpu = Gpu::open(cli.adapter).unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(1);
    });
    println!("GPU: {}", gpu.description);
    let batch = (cli.batch / WORKGROUP).max(1) * WORKGROUP;
    let batch = batch.min(gpu.max_batch());

    if cli.self_test {
        std::process::exit(if self_test(&gpu) { 0 } else { 1 });
    }
    if let Some(secs) = cli.bench {
        bench(&gpu, batch, secs);
        return;
    }
    let Some(miner) = cli.miner.as_deref() else {
        eprintln!("--miner <hex address> is required to mine (or use --list, --self-test, --bench)");
        std::process::exit(2);
    };
    if hex::decode(miner).map(|b| b.len()) != Ok(32) {
        eprintln!("--miner must be a 32-byte hex address");
        std::process::exit(2);
    }
    mine(&gpu, &cli.node, miner, batch, Duration::from_secs(cli.refresh));
}

fn random_header(rng: &mut impl Rng) -> [u8; HEADER_LEN] {
    let mut h = [0u8; HEADER_LEN];
    rng.fill(&mut h[..]);
    h
}

/// The GPU's smallest qualifying nonce must equal the first one a CPU scan of
/// the same range finds, for random headers at an easy target.
fn self_test(gpu: &Gpu) -> bool {
    let mut rng = rand::thread_rng();
    let count = 1u32 << 16;
    // About one hash in 4096 meets this target, so each range has ~16 hits.
    let mut target = [0xffu8; 32];
    target[0] = 0x00;
    target[1] = 0x10;
    let mut ok = true;
    for case in 0..12 {
        let header = random_header(&mut rng);
        let hi: u32 = rng.gen();
        // Include ranges that end exactly at the top of the low word.
        let lo_base = if case % 4 == 3 {
            u32::MAX - (count - 1)
        } else {
            rng.gen_range(0..=u32::MAX - count) & !0xff
        };
        let work = gpu_work(&header, &target);
        let got = gpu.search(&work, hi, lo_base, count);
        let want = (0..count).map(|i| lo_base + i).find(|&lo| {
            meets_target(&header, ((hi as u64) << 32) | lo as u64, &target)
        });
        let pass = got == want;
        ok &= pass;
        println!(
            "case {case:2}: gpu {got:?} cpu {want:?} {}",
            if pass { "ok" } else { "MISMATCH" }
        );
    }
    // A target no hash can meet: the GPU must report nothing.
    let header = random_header(&mut rng);
    let none = gpu.search(&gpu_work(&header, &[0u8; 32]), 0, 0, count);
    println!("zero target: gpu {none:?} {}", if none.is_none() { "ok" } else { "MISMATCH" });
    ok &= none.is_none();
    println!("{}", if ok { "SELF-TEST PASSED" } else { "SELF-TEST FAILED" });
    ok
}

fn bench(gpu: &Gpu, batch: u32, secs: u64) {
    let mut rng = rand::thread_rng();
    let work = gpu_work(&random_header(&mut rng), &[0u8; 32]);
    gpu.search(&work, 0, 0, batch); // warm-up: pipeline creation, first submit
    let start = Instant::now();
    let mut hashes = 0u64;
    let mut lo = 0u32;
    while start.elapsed() < Duration::from_secs(secs) {
        if lo.checked_add(batch - 1).is_none() {
            lo = 0;
        }
        gpu.search(&work, 1, lo, batch);
        lo = lo.wrapping_add(batch);
        hashes += batch as u64;
    }
    let rate = hashes as f64 / start.elapsed().as_secs_f64();
    println!("{:.1} MH/s ({} nonces per dispatch)", rate / 1e6, batch);
}

fn mine(gpu: &Gpu, node: &str, miner: &str, batch: u32, refresh: Duration) {
    let http = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("HTTP client");
    let node = node.trim_end_matches('/');
    let mut rng = rand::thread_rng();
    let mut hashes = 0u64;
    let mut rate_start = Instant::now();

    loop {
        let work: Work = match http
            .get(format!("{node}/mining/work/{miner}"))
            .send()
            .and_then(|r| r.error_for_status())
            .and_then(|r| r.json())
        {
            Ok(w) => w,
            Err(e) => {
                eprintln!("[gpu] cannot get work from {node}: {e}; retrying in 5 s");
                std::thread::sleep(Duration::from_secs(5));
                continue;
            }
        };
        let header: [u8; HEADER_LEN] = match hex::decode(&work.header).ok().and_then(|b| b.try_into().ok()) {
            Some(h) => h,
            None => {
                eprintln!("[gpu] node sent a malformed header");
                std::thread::sleep(Duration::from_secs(5));
                continue;
            }
        };
        let target: [u8; 32] = match hex::decode(&work.target).ok().and_then(|b| b.try_into().ok()) {
            Some(t) => t,
            None => {
                eprintln!("[gpu] node sent a malformed target");
                std::thread::sleep(Duration::from_secs(5));
                continue;
            }
        };
        let gw = gpu_work(&header, &target);
        // A random high word, so two GPUs on the same template search
        // different nonces.
        let mut hi: u32 = rng.gen();
        let mut lo = 0u32;
        let fetched = Instant::now();
        let mut last_check = Instant::now();

        loop {
            // Keep each batch inside one range of low words.
            if lo.checked_add(batch - 1).is_none() {
                hi = hi.wrapping_add(1);
                lo = 0;
            }
            let hit = gpu.search(&gw, hi, lo, batch);
            let batch_hi = hi;
            hashes += batch as u64;
            match lo.checked_add(batch) {
                Some(next) => lo = next,
                None => {
                    hi = hi.wrapping_add(1);
                    lo = 0;
                }
            }

            if let Some(found_lo) = hit {
                let nonce = ((batch_hi as u64) << 32) | found_lo as u64;
                if !meets_target(&header, nonce, &target) {
                    eprintln!("[gpu] GPU reported nonce {nonce} but the CPU check fails; discarding");
                    break;
                }
                submit(&http, node, work.job, nonce, work.height);
                break;
            }

            if last_check.elapsed() >= Duration::from_secs(2) {
                last_check = Instant::now();
                if let Ok(info) = http
                    .get(format!("{node}/chain"))
                    .send()
                    .and_then(|r| r.json::<ChainInfo>())
                {
                    if info.height >= work.height {
                        break; // someone else found this height
                    }
                }
                let secs = rate_start.elapsed().as_secs_f64();
                if secs >= 30.0 {
                    println!(
                        "[gpu] {:.1} MH/s, working on height {}",
                        hashes as f64 / secs / 1e6,
                        work.height
                    );
                    hashes = 0;
                    rate_start = Instant::now();
                }
            }
            if fetched.elapsed() >= refresh {
                break;
            }
        }
    }
}

fn submit(http: &reqwest::blocking::Client, node: &str, job: u64, nonce: u64, height: u64) {
    let resp = http
        .post(format!("{node}/mining/submit"))
        .json(&serde_json::json!({ "job": job, "nonce": nonce }))
        .send()
        .and_then(|r| r.json::<serde_json::Value>());
    match resp {
        Ok(v) => println!(
            "[gpu] height {height}: {}",
            v.get("status").and_then(|s| s.as_str()).unwrap_or("?")
        ),
        Err(e) => eprintln!("[gpu] submit failed: {e}"),
    }
}
