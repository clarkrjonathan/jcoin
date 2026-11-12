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
//!   gpu-miner --bench-matrix 3               compare kernels, workgroup sizes, batches
//!   gpu-miner --miner <hex address>          mine against http://127.0.0.1:3000

use std::time::{Duration, Instant};

use clap::Parser;
use rand::Rng;

use jcoin_gpu_miner::gpu::{self, Gpu, WORKGROUP};
use jcoin_gpu_miner::kernel::{Config, Kernel};
use jcoin_gpu_miner::miner::{mine, rest_after, MinerControl};
use jcoin_gpu_miner::pow::{block_hash, gpu_work, meets_target, HEADER_LEN};

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
    /// Nonces per GPU dispatch (rounded to a multiple of the workgroup size).
    /// Larger batches waste less time on overhead but react later to a new
    /// block.
    #[arg(long, default_value_t = 1 << 22)]
    batch: u32,
    /// The shader variant: baseline, unrolled, or precomputed.
    #[arg(long, default_value = "baseline", value_parser = Kernel::parse)]
    kernel: Kernel,
    /// Threads per workgroup (a power of two, at most the GPU's limit).
    #[arg(long, default_value_t = WORKGROUP)]
    workgroup: u32,
    /// Fetch a fresh template at least this often (seconds), to pick up new
    /// transactions and a current timestamp.
    #[arg(long, default_value_t = 20)]
    refresh: u64,
    /// Limit GPU use to about this percent of full power (1 to 100). The miner
    /// pauses between GPU dispatches; the hash rate falls in proportion.
    #[arg(long, default_value_t = 100, value_parser = clap::value_parser!(u32).range(1..=100))]
    limit: u32,
    /// List the GPUs and exit.
    #[arg(long)]
    list: bool,
    /// Compare GPU results with the CPU on random headers and exit.
    #[arg(long)]
    self_test: bool,
    /// Measure the hash rate for this many seconds and exit.
    #[arg(long)]
    bench: Option<u64>,
    /// Compare configurations, this many seconds each, and exit: every
    /// kernel, then workgroup sizes for the fastest kernel, then batch sizes.
    /// Each configuration is checked against the CPU before it is timed.
    #[arg(long)]
    bench_matrix: Option<u64>,
}

fn main() {
    let cli = Cli::parse();
    if cli.list {
        for (i, a) in gpu::adapters().iter().enumerate() {
            println!("{i}: {}", gpu::describe(a));
        }
        return;
    }
    if let Some(secs) = cli.bench_matrix {
        std::process::exit(if bench_matrix(cli.adapter, secs.max(1)) { 0 } else { 1 });
    }
    if !cli.workgroup.is_power_of_two() {
        eprintln!("--workgroup must be a power of two");
        std::process::exit(2);
    }
    let config = Config {
        kernel: cli.kernel,
        workgroup: cli.workgroup,
    };
    let gpu = Gpu::open_with(cli.adapter, config).unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(1);
    });
    println!("GPU: {}", gpu.description);
    if config != Config::default() {
        println!("kernel {}, workgroup {}", config.kernel.name(), config.workgroup);
    }
    let batch = round_batch(&gpu, cli.batch);

    if cli.self_test {
        std::process::exit(if self_test(&gpu, 12, true) { 0 } else { 1 });
    }
    if let Some(secs) = cli.bench {
        let rate = bench(&gpu, batch, secs, cli.limit);
        println!("{:.1} MH/s ({batch} nonces per dispatch, {}% GPU use limit)", rate / 1e6, cli.limit);
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
    let control = MinerControl::default();
    control.set_limit(cli.limit);
    if cli.limit < 100 {
        println!("GPU use limited to {}%", cli.limit);
    }
    mine(
        &gpu,
        &cli.node,
        miner,
        batch,
        Duration::from_secs(cli.refresh),
        &control,
    );
}

fn random_header(rng: &mut impl Rng) -> [u8; HEADER_LEN] {
    let mut h = [0u8; HEADER_LEN];
    rng.fill(&mut h[..]);
    h
}

/// `batch` as a multiple of the GPU's workgroup size, within its limit.
fn round_batch(gpu: &Gpu, batch: u32) -> u32 {
    let wg = gpu.config.workgroup;
    ((batch / wg).max(1) * wg).min(gpu.max_batch())
}

/// The GPU's smallest qualifying nonce must equal the first one a CPU scan of
/// the same range finds, for `cases` random headers. Most cases use an easy
/// target, decided by the first hash word. Every fourth uses the hash of one
/// nonce in the range plus one, so that the hit agrees with the target in
/// its leading words, as every hit at real difficulty does.
fn self_test(gpu: &Gpu, cases: u32, verbose: bool) -> bool {
    let mut rng = rand::thread_rng();
    let count = round_batch(gpu, 1 << 16);
    // About one hash in 4096 meets this target, so each range has ~16 hits.
    let mut easy = [0xffu8; 32];
    easy[0] = 0x00;
    easy[1] = 0x10;
    let mut ok = true;
    for case in 0..cases {
        let header = random_header(&mut rng);
        let hi: u32 = rng.gen();
        // Include ranges that end exactly at the top of the low word.
        let lo_base = if case % 4 == 3 {
            u32::MAX - (count - 1)
        } else {
            rng.gen_range(0..=u32::MAX - count) & !0xff
        };
        let target = if case % 4 == 1 {
            let lo = lo_base + rng.gen_range(0..count);
            let mut t = block_hash(&header, ((hi as u64) << 32) | lo as u64);
            // Plus one, as a big-endian number.
            for byte in t.iter_mut().rev() {
                *byte = byte.wrapping_add(1);
                if *byte != 0 {
                    break;
                }
            }
            t
        } else {
            easy
        };
        let work = gpu_work(&header, &target);
        let got = gpu.search(&work, hi, lo_base, count);
        let want = (0..count).map(|i| lo_base + i).find(|&lo| {
            meets_target(&header, ((hi as u64) << 32) | lo as u64, &target)
        });
        let pass = got == want;
        ok &= pass;
        if verbose || !pass {
            println!(
                "case {case:2}: gpu {got:?} cpu {want:?} {}",
                if pass { "ok" } else { "MISMATCH" }
            );
        }
    }
    // A target no hash can meet: the GPU must report nothing.
    let header = random_header(&mut rng);
    let none = gpu.search(&gpu_work(&header, &[0u8; 32]), 0, 0, count);
    ok &= none.is_none();
    if verbose || none.is_some() {
        println!("zero target: gpu {none:?} {}", if none.is_none() { "ok" } else { "MISMATCH" });
    }
    if verbose {
        println!("{}", if ok { "SELF-TEST PASSED" } else { "SELF-TEST FAILED" });
    }
    ok
}

/// Compare configurations on one GPU, `secs` seconds each, in three steps so
/// that it takes about seven runs, not every combination: each kernel at the
/// default workgroup size and batch; other workgroup sizes for the fastest
/// kernel; other batch sizes for the fastest of those. A configuration whose
/// results differ from the CPU's is reported and not timed. Returns false if
/// any failed.
fn bench_matrix(adapter: Option<usize>, secs: u64) -> bool {
    struct Row {
        config: Config,
        batch: u32,
        rate: Option<f64>,
    }
    let mut rows: Vec<Row> = Vec::new();
    let mut described = false;
    let mut run = |config: Config, batch: u32, rows: &mut Vec<Row>| {
        let rate = match Gpu::open_with(adapter, config) {
            Err(e) => {
                println!("{} wg {}: {e}", config.kernel.name(), config.workgroup);
                None
            }
            Ok(gpu) => {
                if !described {
                    println!("GPU: {}; {secs} s per configuration", gpu.description);
                    described = true;
                }
                let batch = round_batch(&gpu, batch);
                if rows.iter().any(|r| r.config == config && r.batch == batch) {
                    return;
                }
                let rate = self_test(&gpu, 8, false).then(|| bench(&gpu, batch, secs, 100));
                rows.push(Row { config, batch, rate });
                return;
            }
        };
        rows.push(Row { config, batch, rate });
    };
    let best = |rows: &[Row]| {
        rows.iter()
            .filter(|r| r.rate.is_some())
            .max_by(|a, b| a.rate.partial_cmp(&b.rate).unwrap())
            .map(|r| (r.config, r.batch))
    };

    let default_batch = 1u32 << 22;
    for kernel in Kernel::ALL {
        run(Config { kernel, workgroup: WORKGROUP }, default_batch, &mut rows);
    }
    if let Some((config, _)) = best(&rows) {
        for workgroup in [64, 128] {
            run(Config { workgroup, ..config }, default_batch, &mut rows);
        }
    }
    if let Some((config, _)) = best(&rows) {
        for batch in [1u32 << 20, 1 << 24] {
            run(config, batch, &mut rows);
        }
    }

    let baseline = rows.first().and_then(|r| r.rate);
    println!("{:<12} {:>9} {:>10} {:>9}  vs baseline", "kernel", "workgroup", "batch", "MH/s");
    for r in &rows {
        let (rate, versus) = match (r.rate, baseline) {
            (Some(rate), Some(base)) => (format!("{:.1}", rate / 1e6), format!("{:+.0}%", 100.0 * (rate / base - 1.0))),
            (Some(rate), None) => (format!("{:.1}", rate / 1e6), "-".to_string()),
            (None, _) => ("FAILED".to_string(), "wrong results or cannot open".to_string()),
        };
        println!(
            "{:<12} {:>9} {:>10} {:>9}  {versus}",
            r.config.kernel.name(),
            r.config.workgroup,
            r.batch,
            rate
        );
    }
    if let Some((config, batch)) = best(&rows) {
        println!(
            "fastest: --kernel {} --workgroup {} --batch {batch}",
            config.kernel.name(),
            config.workgroup
        );
    }
    rows.iter().all(|r| r.rate.is_some())
}

/// The hash rate over about `secs` seconds, in hashes per second.
fn bench(gpu: &Gpu, batch: u32, secs: u64, limit: u32) -> f64 {
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
        let dispatched = Instant::now();
        gpu.search(&work, 1, lo, batch);
        std::thread::sleep(rest_after(dispatched.elapsed(), limit));
        lo = lo.wrapping_add(batch);
        hashes += batch as u64;
    }
    hashes as f64 / start.elapsed().as_secs_f64()
}
