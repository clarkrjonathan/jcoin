//! The mining loop: get a block template from a node, search the nonce on the
//! GPU, check every hit on the CPU, and submit it.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
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
    /// GPU use limit in percent; 0 means not set (no limit).
    limit: AtomicU32,
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

    /// Limit GPU use to about `percent` of full power (clamped to 1..=100).
    /// The mining loop pauses between GPU dispatches to keep to it. Takes
    /// effect at the next dispatch, also while mining.
    pub fn set_limit(&self, percent: u32) {
        self.limit.store(percent.clamp(1, 100), Ordering::Relaxed);
    }

    /// The GPU use limit in percent; 100 if none was set.
    pub fn limit(&self) -> u32 {
        match self.limit.load(Ordering::Relaxed) {
            0 => 100,
            percent => percent,
        }
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

    /// Sleep for `total`, waking early when asked to stop. Sleeps in steps
    /// of at most 100 ms and never past the end: a rest of 10 ms takes 10 ms.
    /// (Until 2.1.2 every step was a full 100 ms, so any GPU use limit below
    /// 100% rested at least 100 ms per dispatch and they all behaved alike.)
    fn pause(&self, total: Duration) {
        let end = Instant::now() + total;
        while !self.stopped() {
            let left = end.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            std::thread::sleep(left.min(Duration::from_millis(100)));
        }
    }
}

/// How long to rest after a GPU dispatch that took `busy`, so that the GPU is
/// busy for `percent` of the time: `busy * (100 - percent) / percent`.
pub fn rest_after(busy: Duration, percent: u32) -> Duration {
    let percent = percent.clamp(1, 100);
    busy * (100 - percent) / percent
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
/// Hashes counted since a start time, read and restarted as a rate.
struct RateWindow {
    start: Instant,
    hashes: u64,
}

impl RateWindow {
    fn new(start: Instant) -> RateWindow {
        RateWindow { start, hashes: 0 }
    }

    fn add(&mut self, hashes: u64) {
        self.hashes += hashes;
    }

    /// Hashes per second since the start, then start again at `now`. `None`
    /// if no time has passed.
    fn take(&mut self, now: Instant) -> Option<u64> {
        let secs = now.saturating_duration_since(self.start).as_secs_f64();
        if secs <= 0.0 {
            return None;
        }
        let rate = (self.hashes as f64 / secs) as u64;
        *self = RateWindow::new(now);
        Some(rate)
    }
}

pub fn mine(gpu: &Gpu, node: &str, miner: &str, batch: u32, refresh: Duration, control: &MinerControl) {
    let http = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .expect("HTTP client");
    let node = node.trim_end_matches('/');
    let mut rng = rand::thread_rng();
    let mut hashes = 0u64;
    let mut rate_start = Instant::now();
    // One window for the whole run, across templates: its hashes and its
    // start must both carry over, or a reading after a new template divides
    // the old template's hashes by the new one's time (up to twice the rate).
    let mut window = RateWindow::new(Instant::now());
    let mut last_check = Instant::now();

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
        control.height.store(work.height, Ordering::Relaxed);

        while !control.stopped() {
            // Keep each batch inside one range of low words.
            if lo.checked_add(batch - 1).is_none() {
                hi = hi.wrapping_add(1);
                lo = 0;
            }
            let dispatched = Instant::now();
            let hit = gpu.search(&gw, hi, lo, batch);
            // Under a limit, rest after the dispatch so the GPU is busy for
            // about that share of the time. A found nonce is handled first.
            if hit.is_none() {
                control.pause(rest_after(dispatched.elapsed(), control.limit()));
            }
            let batch_hi = hi;
            hashes += batch as u64;
            window.add(batch as u64);
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
                if let Some(rate) = window.take(Instant::now()) {
                    control.hash_rate.store(rate, Ordering::Relaxed);
                }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_rate_window_divides_its_hashes_by_its_own_time() {
        let t0 = Instant::now();
        let s = Duration::from_secs;
        let mut w = RateWindow::new(t0);
        // 1.5 s on one template, a new template, then 0.5 s more: the
        // reading covers all 2 s, not only the last 0.5 s.
        w.add(1_500);
        w.add(500);
        assert_eq!(w.take(t0 + s(2)), Some(1_000));
        // The next window starts at the reading.
        w.add(4_000);
        assert_eq!(w.take(t0 + s(4)), Some(2_000));
        assert_eq!(w.take(t0 + s(4)), None);
    }

    #[test]
    fn rest_keeps_the_gpu_busy_for_the_limit_share() {
        let busy = Duration::from_millis(30);
        assert_eq!(rest_after(busy, 100), Duration::ZERO);
        assert_eq!(rest_after(busy, 50), Duration::from_millis(30));
        assert_eq!(rest_after(busy, 25), Duration::from_millis(90));
        assert_eq!(rest_after(busy, 75), Duration::from_millis(10));
        // Out-of-range values are clamped, never a division by zero.
        assert_eq!(rest_after(busy, 0), rest_after(busy, 1));
        assert_eq!(rest_after(busy, 500), Duration::ZERO);
    }

    #[test]
    fn a_pause_lasts_as_long_as_asked_not_a_fixed_step() {
        let control = MinerControl::default();
        for millis in [0u64, 10, 30, 90, 250] {
            let start = Instant::now();
            control.pause(Duration::from_millis(millis));
            let took = start.elapsed().as_millis() as u64;
            assert!(took >= millis && took < millis + 40, "{millis} ms pause took {took} ms");
        }
        // A stop request ends a long pause early.
        control.stop();
        let start = Instant::now();
        control.pause(Duration::from_secs(5));
        assert!(start.elapsed() < Duration::from_millis(50));
    }

    /// A node that hands out work no GPU can solve (target zero) at height 1
    /// and reports height 0, so `mine` hashes one job for as long as it runs.
    fn unsolvable_node() -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let mut buf = [0u8; 2048];
                let n = stream.read(&mut buf).unwrap_or(0);
                let request = String::from_utf8_lossy(&buf[..n]);
                let body = if request.starts_with("GET /mining/work/") {
                    format!(
                        r#"{{"job":1,"height":1,"header":"{}","target":"{}"}}"#,
                        "00".repeat(HEADER_LEN),
                        "00".repeat(32)
                    )
                } else {
                    r#"{"height":0}"#.to_string()
                };
                let _ = write!(
                    stream,
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
            }
        });
        url
    }

    /// The GPU use limit, measured through the mining loop the app runs (not
    /// `--bench`): the hash rate at 50% and 25% is about that share of 100%.
    #[test]
    #[ignore = "needs a GPU; about 30 s"]
    fn the_limit_scales_the_hash_rate_of_the_mining_loop() {
        let gpu = crate::gpu::Gpu::open(None).expect("a GPU");
        let batch = ((1u32 << 22) / crate::gpu::WORKGROUP * crate::gpu::WORKGROUP).min(gpu.max_batch());
        let node = unsolvable_node();
        let control = std::sync::Arc::new(MinerControl::default());
        let rate_at = |percent: u32| {
            control.set_limit(percent);
            // Let one 2 s window pass under the new limit, then average three.
            std::thread::sleep(Duration::from_millis(2_500));
            let mut sum = 0u64;
            for _ in 0..3 {
                std::thread::sleep(Duration::from_secs(2));
                sum += control.hash_rate.load(Ordering::Relaxed);
            }
            sum as f64 / 3.0
        };
        std::thread::scope(|scope| {
            let c = control.clone();
            let (gpu, node) = (&gpu, &node);
            scope.spawn(move || mine(gpu, node, &"11".repeat(32), batch, Duration::from_secs(600), &c));
            let full = rate_at(100);
            let half = rate_at(50);
            let quarter = rate_at(25);
            control.stop();
            println!(
                "hash rate: 100% {:.1} MH/s, 50% {:.1} MH/s ({:.2}), 25% {:.1} MH/s ({:.2})",
                full / 1e6,
                half / 1e6,
                half / full,
                quarter / 1e6,
                quarter / full
            );
            assert!(full > 0.0, "no hash rate at 100%");
            assert!((0.35..0.65).contains(&(half / full)), "50% gave {:.2} of full", half / full);
            assert!((0.15..0.35).contains(&(quarter / full)), "25% gave {:.2} of full", quarter / full);
        });
    }

    #[test]
    fn the_limit_defaults_to_full_power_and_is_clamped() {
        let control = MinerControl::default();
        assert_eq!(control.limit(), 100);
        control.set_limit(40);
        assert_eq!(control.limit(), 40);
        control.set_limit(0);
        assert_eq!(control.limit(), 1);
        control.set_limit(250);
        assert_eq!(control.limit(), 100);
    }
}
