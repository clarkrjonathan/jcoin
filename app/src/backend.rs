//! Everything the window drives: the embedded node, a poller that reads the
//! node's REST endpoints, the wallet key, and the GPU mining thread.
//!
//! The window never blocks on the network. Background threads write what they
//! learn into shared state, and the window reads a copy each frame.

use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use ed25519_dalek::SigningKey;
use jcoin::amount::{format_jef, parse_jef};
use jcoin::service::{self, ServiceConfig};
use jcoin::wallet;
use jcoin_gpu_miner::gpu::{self, Gpu, WORKGROUP};
use jcoin_gpu_miner::miner::{mine, FoundBlock, MinerControl};

use crate::applog::{self, AppLog};

/// The public relay every app instance uses to find and reach other nodes.
const DEFAULT_RELAY: &str =
    "/ip4/3.14.123.27/tcp/9000/p2p/12D3KooWRGZbiiQPP4XRPttimhHQVGEAfkEokYoPRPE3g6feKL1v";

/// Name of the key created on first run.
const FIRST_KEY: &str = "wallet";

/// File in the data folder that records which key is in use.
const ACTIVE_KEY_FILE: &str = "active_key";

/// Where and how this instance runs. Environment variables override the
/// defaults, for testing: `JCOIN_APP_DATA` (data folder), `JCOIN_APP_RELAY` (a
/// relay multiaddr, or `none`), `JCOIN_APP_PEER` (a peer to dial), and
/// `JCOIN_APP_REGTEST=1` (the local test network).
pub struct Settings {
    pub data_dir: PathBuf,
    pub relay: Option<String>,
    pub peers: Vec<String>,
    pub regtest: bool,
}

impl Settings {
    pub fn from_env() -> Settings {
        let var = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
        let regtest = var("JCOIN_APP_REGTEST").is_some_and(|v| v == "1");
        Settings {
            data_dir: var("JCOIN_APP_DATA")
                .map(PathBuf::from)
                .unwrap_or_else(|| default_data_dir(regtest)),
            relay: match var("JCOIN_APP_RELAY") {
                Some(v) if v == "none" => None,
                Some(v) => Some(v),
                None => Some(DEFAULT_RELAY.to_string()),
            },
            peers: var("JCOIN_APP_PEER").into_iter().collect(),
            regtest,
        }
    }
}

/// The per-user data folder: `%APPDATA%\JeffyCoin` on Windows,
/// `~/Library/Application Support/JeffyCoin` on macOS, and
/// `$XDG_DATA_HOME/jeffycoin` (or `~/.local/share/jeffycoin`) elsewhere. The
/// test network gets its own folder so the two chains never mix.
fn default_data_dir(regtest: bool) -> PathBuf {
    let env = |name: &str| std::env::var_os(name).map(PathBuf::from);
    let base = if cfg!(windows) {
        env("APPDATA").map(|p| p.join("JeffyCoin"))
    } else if cfg!(target_os = "macos") {
        env("HOME").map(|p| p.join("Library/Application Support/JeffyCoin"))
    } else {
        env("XDG_DATA_HOME")
            .or_else(|| env("HOME").map(|p| p.join(".local/share")))
            .map(|p| p.join("jeffycoin"))
    };
    let base = base.unwrap_or_else(|| PathBuf::from("jeffycoin-data"));
    if regtest {
        base.join("regtest")
    } else {
        base
    }
}

/// `preferred` if nothing is listening on it, otherwise a free port the
/// operating system picks. Lets a second instance start next to a first one.
fn free_port(host: &str, preferred: u16) -> u16 {
    if TcpListener::bind((host, preferred)).is_ok() {
        return preferred;
    }
    TcpListener::bind((host, 0))
        .and_then(|l| l.local_addr())
        .map(|a| a.port())
        .unwrap_or(preferred)
}

/// What the poller last read from the node.
#[derive(Clone, Default)]
pub struct NodeView {
    /// The REST server has answered at least once.
    pub online: bool,
    pub height: u64,
    pub peers: u64,
    /// Confirmed balance, base units.
    pub balance: u64,
    /// Base units committed to this wallet's pending transactions.
    pub pending: u64,
    /// Confirmed nonce: the number of this wallet's transfers in the chain.
    pub nonce: u64,
    /// The account figures above have been read for the key in use.
    pub account_known: bool,
    /// The tip's difficulty target, as hex.
    pub target: String,
    /// This node's own addresses, as it advertises them.
    pub advertised: Vec<String>,
    pub connections: Vec<Connection>,
    /// Set if the node stopped (for example, its ports could not be opened).
    pub fatal: Option<String>,
}

/// One open connection, as `GET /peers` reports it.
#[derive(Clone, Default)]
pub struct Connection {
    pub peer: String,
    pub address: String,
    pub outbound: bool,
    pub relayed: bool,
    pub is_relay: bool,
    /// Blocks and transactions are exchanged with this peer.
    pub session: bool,
    /// When it opened (Unix seconds).
    pub since: u64,
}

/// A transfer sent from this window.
#[derive(Clone)]
pub struct SentTx {
    /// Address of the key that sent it.
    pub sender: String,
    pub id: String,
    pub recipient: String,
    pub amount: u64,
    pub nonce: u64,
}

/// What the mining thread is doing.
#[derive(Clone, Debug, PartialEq)]
pub enum MiningState {
    Idle,
    Starting,
    Running,
    Failed(String),
}

struct Mining {
    control: Arc<MinerControl>,
    thread: JoinHandle<()>,
    /// Set by the thread once the GPU is open, or with the reason it is not.
    opened: Arc<Mutex<Option<Result<String, String>>>>,
}

/// A stored key, for the key list.
#[derive(Clone)]
pub struct KeyEntry {
    pub name: String,
    pub address: String,
}

pub struct Backend {
    pub settings: Settings,
    /// Address of the key in use: it receives mining rewards and signs sends.
    pub address: String,
    /// Name of the key in use.
    pub key_name: String,
    key: SigningKey,
    /// The address the poller reads the account of.
    polled_address: Arc<Mutex<String>>,
    keys: Vec<KeyEntry>,
    pub log: Arc<AppLog>,
    /// Blocks mined in mining runs that have ended.
    won: Vec<FoundBlock>,
    node_url: String,
    view: Arc<Mutex<NodeView>>,
    http: reqwest::blocking::Client,
    mining: Option<Mining>,
    mining_error: Option<String>,
    /// GPU names by adapter index; filled in by a background thread.
    gpus: Arc<Mutex<Option<Vec<String>>>>,
    pub sent: Vec<SentTx>,
}

impl Backend {
    /// Load (or create) the wallet key, then start the node and the poller.
    pub fn start(settings: Settings) -> Result<Backend, String> {
        std::fs::create_dir_all(&settings.data_dir)
            .map_err(|e| format!("cannot create {}: {e}", settings.data_dir.display()))?;
        // Use the key recorded as active, else the first stored key, else
        // create one: a first run needs no setup.
        let stored = wallet::list_keys(&settings.data_dir);
        let recorded = std::fs::read_to_string(settings.data_dir.join(ACTIVE_KEY_FILE))
            .map(|s| s.trim().to_string())
            .ok()
            .filter(|name| stored.contains(name));
        let key_name = recorded
            .or_else(|| stored.first().cloned())
            .unwrap_or_else(|| FIRST_KEY.to_string());
        let key = wallet::load_or_generate_key(&settings.data_dir, &key_name)?;
        let address = hex::encode(wallet::address_of(&key));

        // Route the node's output into the log file and the window. Only the
        // first backend in a process can do this.
        let log = AppLog::open(&settings.data_dir);
        {
            let log = log.clone();
            jcoin::log::set_sink(move |level, line| {
                log.push(level == jcoin::log::Level::Error, line);
            });
        }
        log.push(
            false,
            &format!(
                "[app] JeffyCoin {} starting on {} ({}), key '{key_name}' {address}",
                env!("CARGO_PKG_VERSION"),
                if settings.regtest { "regtest" } else { "mainnet" },
                std::env::consts::OS,
            ),
        );

        let rest_port = free_port("127.0.0.1", 3000);
        let p2p_port = free_port("0.0.0.0", 8333);
        let node_url = format!("http://127.0.0.1:{rest_port}");
        let config = ServiceConfig {
            rest_addr: format!("127.0.0.1:{rest_port}"),
            p2p_addr: format!("0.0.0.0:{p2p_port}"),
            peers: settings.peers.clone(),
            no_listen: false,
            advertise: Vec::new(),
            max_peers: 8,
            relay: settings.relay.clone(),
            key_file: None,
            cpu_miner: None,
            data_dir: Some(settings.data_dir.clone()),
            regtest: settings.regtest,
        };

        let view = Arc::new(Mutex::new(NodeView::default()));
        spawn_node(config, view.clone());

        let http = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .map_err(|e| format!("cannot create the HTTP client: {e}"))?;
        let polled_address = Arc::new(Mutex::new(address.clone()));
        spawn_poller(http.clone(), node_url.clone(), polled_address.clone(), view.clone());

        let gpus = Arc::new(Mutex::new(None));
        {
            let gpus = gpus.clone();
            std::thread::spawn(move || {
                let names = gpu::adapters().iter().map(gpu::describe).collect();
                *gpus.lock().unwrap() = Some(names);
            });
        }

        let mut backend = Backend {
            settings,
            address,
            key_name,
            key,
            polled_address,
            keys: Vec::new(),
            log,
            won: Vec::new(),
            node_url,
            view,
            http,
            mining: None,
            mining_error: None,
            gpus,
            sent: Vec::new(),
        };
        backend.refresh_keys();
        Ok(backend)
    }

    /// Reread the stored keys from the data folder.
    fn refresh_keys(&mut self) {
        let dir = &self.settings.data_dir;
        self.keys = wallet::list_keys(dir)
            .into_iter()
            .filter_map(|name| {
                let key = wallet::load_key(dir, &name).ok()?;
                Some(KeyEntry {
                    address: hex::encode(wallet::address_of(&key)),
                    name,
                })
            })
            .collect();
    }

    /// The stored keys, sorted by name.
    pub fn keys(&self) -> &[KeyEntry] {
        &self.keys
    }

    /// Create a new key named `name`. It is stored but not put in use.
    pub fn create_key(&mut self, name: &str) -> Result<(), String> {
        wallet::generate_key(&self.settings.data_dir, name.trim())?;
        self.refresh_keys();
        Ok(())
    }

    /// Store an existing key under `name`, from its secret key (64 hex
    /// characters). It is stored but not put in use.
    pub fn import_key(&mut self, name: &str, secret_hex: &str) -> Result<(), String> {
        wallet::import_key(&self.settings.data_dir, name.trim(), secret_hex)?;
        self.refresh_keys();
        Ok(())
    }

    /// Put the key `name` in use: the balance shown, the address that receives
    /// mining rewards, and the key that signs sends. Refused while mining,
    /// because the running miner pays the address it was started with.
    pub fn select_key(&mut self, name: &str) -> Result<(), String> {
        if self.mining.is_some() {
            return Err("stop mining before switching keys".to_string());
        }
        let key = wallet::load_key(&self.settings.data_dir, name)?;
        self.address = hex::encode(wallet::address_of(&key));
        self.key = key;
        self.key_name = name.to_string();
        // The figures on screen belong to the old key; clear them until the
        // poller has read the new account.
        *self.polled_address.lock().unwrap() = self.address.clone();
        {
            let mut v = self.view.lock().unwrap();
            v.account_known = false;
            (v.balance, v.pending, v.nonce) = (0, 0, 0);
        }
        std::fs::write(self.settings.data_dir.join(ACTIVE_KEY_FILE), name)
            .map_err(|e| format!("the key is in use, but the choice could not be saved: {e}"))
    }

    /// The secret key of the key in use, as 64 hex characters, for a backup.
    /// Whoever has it can spend from the address.
    pub fn secret_hex(&self) -> String {
        hex::encode(self.key.to_bytes())
    }

    pub fn view(&self) -> NodeView {
        self.view.lock().unwrap().clone()
    }

    /// GPU names by adapter index, or `None` while they are being listed.
    pub fn gpus(&self) -> Option<Vec<String>> {
        self.gpus.lock().unwrap().clone()
    }

    pub fn key_file(&self) -> PathBuf {
        wallet::key_path(&self.settings.data_dir, &self.key_name)
    }

    /// Sign a transfer and submit it to the embedded node. `amount` and `fee`
    /// are decimal JEF. Returns the transaction id.
    pub fn send(&mut self, recipient: &str, amount: &str, fee: &str) -> Result<String, String> {
        let to = wallet::decode_address(recipient)?;
        let amount = parse_jef(amount.trim()).map_err(|e| format!("amount: {e}"))?;
        let fee = match fee.trim() {
            "" => 0,
            text => parse_jef(text).map_err(|e| format!("fee: {e}"))?,
        };
        if amount == 0 {
            return Err("amount must be more than zero".into());
        }

        // Read the account from the node now, not from the last poll: signing
        // with a stale nonce would get the transfer rejected.
        let account: serde_json::Value = self
            .http
            .get(format!("{}/balance/{}", self.node_url, self.address))
            .send()
            .and_then(|r| r.error_for_status())
            .and_then(|r| r.json())
            .map_err(|e| format!("the node did not answer: {e}"))?;
        let field = |name: &str| {
            account[name]
                .as_u64()
                .ok_or_else(|| format!("node reply has no '{name}' field"))
        };
        let (balance, pending, nonce) = (field("balance")?, field("pending")?, field("next_nonce")?);
        let needed = amount.checked_add(fee).ok_or("amount + fee is too large")?;
        let available = balance.saturating_sub(pending);
        if available < needed {
            return Err(format!(
                "balance too low: need {} JEF, have {} JEF available",
                format_jef(needed),
                format_jef(available)
            ));
        }

        let tx = wallet::sign_transfer(&self.key, to, amount, fee, nonce);
        let reply: serde_json::Value = self
            .http
            .post(format!("{}/wallet/transaction", self.node_url))
            .json(&tx)
            .send()
            .and_then(|r| r.json())
            .map_err(|e| format!("the node did not answer: {e}"))?;
        if reply["status"] != "accepted" {
            let reason = reply["reason"].as_str().unwrap_or("no reason given");
            return Err(format!("the node rejected the transfer: {reason}"));
        }
        let id = tx.tx_id_hex();
        self.sent.push(SentTx {
            sender: self.address.clone(),
            id: id.clone(),
            recipient: hex::encode(to),
            amount,
            nonce,
        });
        Ok(id)
    }

    /// Start mining on GPU `adapter` (an index into `gpus()`), or on the one
    /// the system prefers for `None`. Rewards go to this wallet.
    pub fn start_mining(&mut self, adapter: Option<usize>) {
        if self.mining.is_some() {
            return;
        }
        self.mining_error = None;
        self.log.push(false, &format!("[app] mining started, rewards to {}", self.address));
        let control = Arc::new(MinerControl::default());
        let opened = Arc::new(Mutex::new(None));
        let thread = {
            let (control, opened) = (control.clone(), opened.clone());
            let (node, miner) = (self.node_url.clone(), self.address.clone());
            std::thread::spawn(move || {
                let gpu = match Gpu::open(adapter) {
                    Ok(gpu) => gpu,
                    Err(e) => {
                        *opened.lock().unwrap() = Some(Err(e));
                        return;
                    }
                };
                *opened.lock().unwrap() = Some(Ok(gpu.description.clone()));
                let batch = ((1u32 << 22) / WORKGROUP * WORKGROUP).min(gpu.max_batch());
                mine(&gpu, &node, &miner, batch, Duration::from_secs(20), &control);
            })
        };
        self.mining = Some(Mining {
            control,
            thread,
            opened,
        });
    }

    /// Ask the mining thread to stop. It returns within one GPU dispatch.
    pub fn stop_mining(&mut self) {
        self.pump_miner();
        if let Some(m) = self.mining.take() {
            m.control.stop();
            self.won.extend(m.control.found());
            self.log.push(false, "[app] mining stopped");
        }
    }

    /// Move the miner's new log lines into the app log. Call once a frame.
    pub fn pump_miner(&mut self) {
        if let Some(m) = &self.mining {
            for (error, text) in m.control.take_events() {
                self.log.push(error, &text);
            }
        }
    }

    /// Blocks mined since the app started, oldest first.
    pub fn won_blocks(&self) -> Vec<FoundBlock> {
        let mut all = self.won.clone();
        if let Some(m) = &self.mining {
            all.extend(m.control.found());
        }
        all
    }

    /// A plain-text report for a bug report: versions, the node's state, its
    /// connections, and the end of the log. It holds addresses but no secrets.
    pub fn diagnostics(&self) -> String {
        let v = self.view();
        let mut out = format!(
            "JeffyCoin {} on {} {}\nnetwork: {}\nrelay: {}\nnode: {} online={} height={} peers={} target={}\naddress: {}\n",
            env!("CARGO_PKG_VERSION"),
            std::env::consts::OS,
            std::env::consts::ARCH,
            if self.settings.regtest { "regtest" } else { "mainnet" },
            self.settings.relay.as_deref().unwrap_or("none"),
            self.node_url,
            v.online,
            v.height,
            v.peers,
            v.target,
            self.address,
        );
        if let Some(reason) = &v.fatal {
            out.push_str(&format!("node stopped: {reason}\n"));
        }
        out.push_str("\nown addresses:\n");
        for a in &v.advertised {
            out.push_str(&format!("  {a}\n"));
        }
        out.push_str("\nconnections:\n");
        for c in &v.connections {
            out.push_str(&format!(
                "  {} {} {} session={} since={} {}\n",
                c.peer,
                if c.outbound { "out" } else { "in" },
                connection_kind(c),
                c.session,
                applog::utc(c.since),
                c.address,
            ));
        }
        out.push_str("\nblocks mined:\n");
        for b in self.won_blocks() {
            out.push_str(&format!("  {} {} {}\n", b.height, applog::utc(b.timestamp), b.hash));
        }
        out.push_str("\nlog (UTC):\n");
        let tail = self.log.tail();
        for line in tail.iter().skip(tail.len().saturating_sub(200)) {
            let level = if line.error { "ERROR" } else { "info " };
            out.push_str(&format!("{} {level} {}\n", applog::utc(line.time), line.text));
        }
        out
    }

    pub fn mining_state(&mut self) -> MiningState {
        let Some(m) = &self.mining else {
            return match &self.mining_error {
                Some(e) => MiningState::Failed(e.clone()),
                None => MiningState::Idle,
            };
        };
        let opened = m.opened.lock().unwrap().clone();
        match opened {
            Some(Err(e)) => {
                self.mining = None;
                self.log.push(true, &format!("[app] cannot open the GPU: {e}"));
                self.mining_error = Some(e.clone());
                MiningState::Failed(e)
            }
            Some(Ok(_)) if m.thread.is_finished() => {
                self.won.extend(m.control.found());
                self.mining = None;
                let e = "the mining thread stopped unexpectedly".to_string();
                self.log.push(true, &format!("[app] {e}"));
                self.mining_error = Some(e.clone());
                MiningState::Failed(e)
            }
            Some(Ok(_)) => MiningState::Running,
            None => MiningState::Starting,
        }
    }

    /// The mining state without the bookkeeping `mining_state` does.
    pub fn mining_state_peek(&self) -> MiningState {
        match (&self.mining, &self.mining_error) {
            (Some(_), _) => MiningState::Running,
            (None, Some(e)) => MiningState::Failed(e.clone()),
            (None, None) => MiningState::Idle,
        }
    }

    /// Live figures of the running miner: hashes per second, the height being
    /// worked on, blocks accepted this run, and the last event.
    pub fn mining_stats(&self) -> Option<(u64, u64, u64, String)> {
        self.mining.as_ref().map(|m| {
            (
                m.control.hash_rate.load(Ordering::Relaxed),
                m.control.height.load(Ordering::Relaxed),
                m.control.blocks_found.load(Ordering::Relaxed),
                m.control.status(),
            )
        })
    }
}

/// Run the node on its own thread and runtime. If it stops, record why.
fn spawn_node(config: ServiceConfig, view: Arc<Mutex<NodeView>>) {
    std::thread::spawn(move || {
        let result = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("cannot start the async runtime: {e}"))
            .and_then(|rt| rt.block_on(service::run(config)));
        let reason = match result {
            Ok(()) => "the node stopped".to_string(),
            Err(e) => e,
        };
        view.lock().unwrap().fatal = Some(reason);
    });
}

/// Read the chain and this wallet's account from the node every two seconds.
fn spawn_poller(
    http: reqwest::blocking::Client,
    node_url: String,
    address: Arc<Mutex<String>>,
    view: Arc<Mutex<NodeView>>,
) {
    std::thread::spawn(move || loop {
        let polled = address.lock().unwrap().clone();
        let get = |path: String| -> Option<serde_json::Value> {
            http.get(format!("{node_url}{path}"))
                .send()
                .and_then(|r| r.error_for_status())
                .and_then(|r| r.json())
                .ok()
        };
        let chain = get("/chain".to_string());
        let account = get(format!("/balance/{polled}"));
        let peers = get("/peers".to_string());
        {
            let mut v = view.lock().unwrap();
            if let Some(c) = &chain {
                v.online = true;
                v.height = c["height"].as_u64().unwrap_or(0);
                v.peers = c["peers"].as_u64().unwrap_or(0);
                v.target = c["target"].as_str().unwrap_or_default().to_string();
            }
            if let Some(p) = &peers {
                let text = |v: &serde_json::Value| v.as_str().unwrap_or_default().to_string();
                v.advertised = p["advertised"].as_array().into_iter().flatten().map(text).collect();
                v.connections = p["connections"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|c| Connection {
                        peer: text(&c["peer"]),
                        address: text(&c["address"]),
                        outbound: c["outbound"].as_bool().unwrap_or(false),
                        relayed: c["relayed"].as_bool().unwrap_or(false),
                        is_relay: c["is_relay"].as_bool().unwrap_or(false),
                        session: c["session"].as_bool().unwrap_or(false),
                        since: c["since"].as_u64().unwrap_or(0),
                    })
                    .collect();
            }
            // Drop the reply if the key was switched while it was in flight.
            let current = *address.lock().unwrap() == polled;
            if let (Some(a), true) = (&account, current) {
                v.account_known = true;
                v.balance = a["balance"].as_u64().unwrap_or(0);
                v.pending = a["pending"].as_u64().unwrap_or(0);
                v.nonce = a["nonce"].as_u64().unwrap_or(0);
            }
        }
        std::thread::sleep(Duration::from_secs(2));
    });
}

/// What kind of connection `c` is, in words.
pub fn connection_kind(c: &Connection) -> &'static str {
    if c.is_relay {
        "the relay"
    } else if c.relayed {
        "through the relay"
    } else {
        "direct"
    }
}

/// Leading zero bits of a hex target: a rough difficulty figure. Each extra
/// bit doubles the expected work per block.
pub fn zero_bits(target_hex: &str) -> u32 {
    let mut bits = 0;
    for c in target_hex.chars() {
        let nibble = c.to_digit(16).unwrap_or(0);
        if nibble == 0 {
            bits += 4;
        } else {
            return bits + nibble.leading_zeros() - 28;
        }
    }
    bits
}

/// Shorten a 64-character hex string for display.
pub fn short(hex: &str) -> String {
    if hex.len() <= 20 {
        return hex.to_string();
    }
    format!("{}…{}", &hex[..10], &hex[hex.len() - 8..])
}

pub fn display_path(path: &Path) -> String {
    path.display().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn wait_for(what: &str, limit: Duration, mut done: impl FnMut() -> bool) {
        let start = Instant::now();
        while !done() {
            assert!(start.elapsed() < limit, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    #[test]
    fn zero_bits_counts_leading_zero_bits() {
        assert_eq!(zero_bits(&format!("0000000f{}", "f".repeat(56))), 28);
        assert_eq!(zero_bits(&format!("000000001f{}", "f".repeat(54))), 35);
        assert_eq!(zero_bits(&"f".repeat(64)), 0);
        assert_eq!(zero_bits(""), 0);
    }

    /// Keys can be created, imported, and switched, and the choice survives a
    /// restart. Needs no GPU.
    #[test]
    fn keys_are_created_switched_and_remembered() {
        let dir = std::env::temp_dir().join(format!("jcoin-app-keys-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let settings = || Settings {
            data_dir: dir.clone(),
            relay: None,
            peers: Vec::new(),
            regtest: true,
        };
        let mut backend = Backend::start(settings()).unwrap();
        assert_eq!(backend.key_name, "wallet");
        assert_eq!(backend.keys().len(), 1);
        let first = backend.address.clone();

        backend.create_key("savings").unwrap();
        assert!(backend.create_key("savings").is_err(), "name taken");
        assert!(backend.create_key("../escape").is_err());
        assert_eq!(backend.address, first, "creating a key does not switch to it");

        backend.select_key("savings").unwrap();
        assert_ne!(backend.address, first);
        assert_eq!(backend.key_file(), dir.join("savings_ed25519"));
        let secret = backend.secret_hex();
        backend.import_key("savings-copy", &secret).unwrap();
        let copy = backend.keys().iter().find(|k| k.name == "savings-copy").unwrap();
        assert_eq!(copy.address, backend.address);
        wait_for("the new account", Duration::from_secs(20), || backend.view().account_known);

        // A second start in the same folder uses the remembered key.
        let again = Backend::start(settings()).unwrap();
        assert_eq!(again.key_name, "savings");
        assert_eq!(again.address, backend.address);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The path the window's buttons drive, end to end on a private regtest
    /// chain: start the node, mine a block on the GPU, send coins, and see the
    /// transfer confirmed. Needs a GPU, so it only runs with `--ignored`.
    #[test]
    #[ignore = "needs a GPU and takes up to a few minutes"]
    fn mine_then_send_on_a_private_chain() {
        let dir = std::env::temp_dir().join(format!("jcoin-app-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut backend = Backend::start(Settings {
            data_dir: dir.clone(),
            relay: None,
            peers: Vec::new(),
            regtest: true,
        })
        .unwrap();
        wait_for("the node", Duration::from_secs(20), || backend.view().online);
        assert!(backend.send(&"11".repeat(32), "1", "").is_err(), "no funds yet");

        backend.start_mining(None);
        wait_for("a mined block", Duration::from_secs(120), || {
            assert!(!matches!(backend.mining_state(), MiningState::Failed(_)));
            backend.view().balance > 0
        });
        backend.stop_mining();
        assert_eq!(backend.mining_state(), MiningState::Idle);
        let won = backend.won_blocks();
        assert!(!won.is_empty(), "the mined block is recorded");
        assert_eq!(won[0].height, 1);
        assert_eq!(won[0].hash.len(), 64);
        assert!(won[0].reward > 0 && won[0].timestamp > 0);
        assert!(backend.log.tail().iter().any(|l| l.text.contains("height 1: accepted")));
        let report = backend.diagnostics();
        assert!(report.contains("network: regtest") && report.contains(&won[0].hash));
        assert!(!report.contains(&backend.secret_hex()), "no secret in the report");

        let before = backend.view().nonce;
        let id = backend.send(&"11".repeat(32), "1.5", "0.1").unwrap();
        assert_eq!(id.len(), 64);
        wait_for("the pending amount", Duration::from_secs(10), || backend.view().pending > 0);

        backend.start_mining(None);
        wait_for("the transfer to confirm", Duration::from_secs(300), || {
            backend.view().nonce > before
        });
        backend.stop_mining();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
