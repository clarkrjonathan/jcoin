//! The mining loop: get a block template from a node, search the nonce on the
//! GPU, check every hit on the CPU, and submit it.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use rand::Rng;
use serde::Deserialize;

use crate::gpu::Gpu;
use crate::pow::{gpu_work, meets_target, HEADER_LEN};

/// A block this miner found and the node accepted.
#[derive(Debug, Clone)]
pub struct FoundBlock {
    pub height: u64,
    pub hash: String,
    /// What the coinbase pays: subsidy plus fees, in base units.
    pub reward: u64,
    /// Transfers in the block, not counting the coinbase.
    pub transfers: u64,
    /// The target the block met, as hex.
    pub target: String,
    /// The block's timestamp (Unix seconds).
    pub timestamp: u64,
}

/// Shared between a running `mine` call and whoever started it: a stop flag
/// in, live figures out.
#[derive(Default)]
pub struct MinerControl {
    stop: AtomicBool,
    /// Hash rate over the last couple of seconds, in hashes per second.
    pub hash_rate: AtomicU64,
    /// Height of the block being worked on.
    pub height: AtomicU64,
    /// Blocks this run has had accepted by the node.
    pub blocks_found: AtomicU64,
    status: Mutex<String>,
    found: Mutex<Vec<FoundBlock>>,
    events: Mutex<Vec<(bool, String)>>,
}

impl MinerControl {
    /// Ask the mining loop to return. It does so within one GPU dispatch.
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    pub fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }

    /// The most recent event (a submitted block, or a failure to reach the node).
    pub fn status(&self) -> String {
        self.status.lock().map(|s| s.clone()).unwrap_or_default()
    }

    /// The blocks found so far, oldest first.
    pub fn found(&self) -> Vec<FoundBlock> {
        self.found.lock().map(|f| f.clone()).unwrap_or_default()
    }

    /// Take the events logged since the last call, as `(is_error, text)`.
    pub fn take_events(&self) -> Vec<(bool, String)> {
        self.events.lock().map(|mut e| std::mem::take(&mut *e)).unwrap_or_default()
    }

    fn set_status(&self, text: String) {
        if let Ok(mut s) = self.status.lock() {
            *s = text;
        }
    }

    /// Print a line and keep it for `take_events`.
    fn log(&self, is_error: bool, text: String) {
        if is_error {
            eprintln!("[gpu] {text}");
        } else {
            println!("[gpu] {text}");
        }
        if let Ok(mut events) = self.events.lock() {
            // Bounded, in case nobody takes the events.
            if events.len() < 1000 {
                events.push((is_error, format!("[gpu] {text}")));
            }
        }
    }

    /// Sleep for `total`, waking early when asked to stop.
    fn pause(&self, total: Duration) {
        let end = Instant::now() + total;
        while !self.stopped() && Instant::now() < end {
            std::thread::sleep(Duration::from_millis(100));
        }
    }
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

/// Mine against `node` until `control.stop` is set, crediting `miner` (a hex
/// address). Progress is printed and also published through `control`.
pub fn mine(gpu: &Gpu, node: &str, miner: &str, batch: u32, refresh: Duration, control: &MinerControl) {
    let http = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("HTTP client");
    let node = node.trim_end_matches('/');
    let mut rng = rand::thread_rng();
    let mut hashes = 0u64;
    let mut rate_start = Instant::now();
    let mut window_hashes = 0u64;

    while !control.stopped() {
        let work: Work = match http
            .get(format!("{node}/mining/work/{miner}"))
            .send()
            .and_then(|r| r.error_for_status())
            .and_then(|r| r.json())
        {
            Ok(w) => w,
            Err(e) => {
                control.log(true, format!("cannot get work from {node}: {e}; retrying in 5 s"));
                control.set_status(format!("cannot get work from the node: {e}"));
                control.pause(Duration::from_secs(5));
                continue;
            }
        };
        let header: [u8; HEADER_LEN] = match hex::decode(&work.header).ok().and_then(|b| b.try_into().ok()) {
            Some(h) => h,
            None => {
                control.log(true, "node sent a malformed header".to_string());
                control.pause(Duration::from_secs(5));
                continue;
            }
        };
        let target: [u8; 32] = match hex::decode(&work.target).ok().and_then(|b| b.try_into().ok()) {
            Some(t) => t,
            None => {
                control.log(true, "node sent a malformed target".to_string());
                control.pause(Duration::from_secs(5));
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
        control.height.store(work.height, Ordering::Relaxed);

        while !control.stopped() {
            // Keep each batch inside one range of low words.
            if lo.checked_add(batch - 1).is_none() {
                hi = hi.wrapping_add(1);
                lo = 0;
            }
            let hit = gpu.search(&gw, hi, lo, batch);
            let batch_hi = hi;
            hashes += batch as u64;
            window_hashes += batch as u64;
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
                    control.log(
                        true,
                        format!("GPU reported nonce {nonce} but the CPU check fails; discarding"),
                    );
                    break;
                }
                submit(&http, node, work.job, nonce, work.height, control);
                break;
            }

            if last_check.elapsed() >= Duration::from_secs(2) {
                let window = last_check.elapsed().as_secs_f64();
                control
                    .hash_rate
                    .store((window_hashes as f64 / window) as u64, Ordering::Relaxed);
                window_hashes = 0;
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
    control.hash_rate.store(0, Ordering::Relaxed);
}

fn submit(
    http: &reqwest::blocking::Client,
    node: &str,
    job: u64,
    nonce: u64,
    height: u64,
    control: &MinerControl,
) {
    let resp = http
        .post(format!("{node}/mining/submit"))
        .json(&serde_json::json!({ "job": job, "nonce": nonce }))
        .send()
        .and_then(|r| r.json::<serde_json::Value>());
    match resp {
        Ok(v) => {
            let status = v.get("status").and_then(|s| s.as_str()).unwrap_or("?");
            control.log(status != "accepted", format!("height {height}: {status}"));
            if status == "accepted" {
                control.blocks_found.fetch_add(1, Ordering::Relaxed);
                let text = |name: &str| v[name].as_str().unwrap_or_default().to_string();
                if let Ok(mut found) = control.found.lock() {
                    found.push(FoundBlock {
                        height,
                        hash: text("hash"),
                        reward: v["reward"].as_u64().unwrap_or(0),
                        transfers: v["transfers"].as_u64().unwrap_or(0),
                        target: text("target"),
                        timestamp: v["timestamp"].as_u64().unwrap_or(0),
                    });
                }
            }
            control.set_status(format!("block at height {height}: {status}"));
        }
        Err(e) => {
            control.log(true, format!("submit failed: {e}"));
            control.set_status(format!("submit failed: {e}"));
        }
    }
}
