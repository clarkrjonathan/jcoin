//! JeffyCoin node binary: a command-line front end for `jcoin::service`.

use clap::Parser;

use jcoin::service::{run, ServiceConfig};

#[derive(Parser)]
#[command(name = "jcoin-node", about = "JeffyCoin node")]
struct Cli {
    /// Address for the wallet-facing REST server.
    #[arg(long, default_value = "0.0.0.0:3000")]
    rest_addr: String,
    /// P2P listen address: `ip:port` (TCP) or a multiaddr.
    #[arg(long, default_value = "0.0.0.0:8333")]
    p2p_addr: String,
    /// Peer to dial on startup and redial while disconnected: `ip:port` or a
    /// multiaddr, optionally ending in `/p2p/<peer id>` (repeatable).
    #[arg(long = "peer")]
    peers: Vec<String>,
    /// Do not listen for inbound connections. The node then dials out only
    /// and, with --relay, is reachable only through the relay (as a node
    /// behind carrier-grade NAT would be).
    #[arg(long)]
    no_listen: bool,
    /// An extra reachable address of this node to advertise, such as a public
    /// address behind port forwarding (repeatable). Listen addresses are
    /// advertised automatically.
    #[arg(long = "advertise")]
    advertise: Vec<String>,
    /// Soft cap on peers before the auto-dialer stops reaching out.
    #[arg(long, default_value_t = 8)]
    max_peers: usize,
    /// A relay for nodes behind NAT, as a multiaddr ending in `/p2p/<relay
    /// peer id>` (the relay prints it at startup). The node reserves a slot on
    /// it, registers with its rendezvous service, and discovers other nodes
    /// there; hole punching upgrades relayed connections to direct ones when
    /// the NATs allow it.
    #[arg(long)]
    relay: Option<String>,
    /// File holding this node's libp2p identity key (created if missing).
    /// Defaults to `<data-dir>/node_key`; without a data dir the identity is
    /// new on every start.
    #[arg(long)]
    key_file: Option<std::path::PathBuf>,
    /// Mine blocks. Requires --miner.
    #[arg(long)]
    mine: bool,
    /// Address (hex public key, 32 bytes) that receives coinbase rewards.
    #[arg(long)]
    miner: Option<String>,
    /// Directory to persist the chain in (an append-only block log). Omit to run
    /// in memory only. Each node needs its own directory.
    #[arg(long)]
    data_dir: Option<std::path::PathBuf>,
    /// Run on the local test network (easy proof of work, its own genesis)
    /// instead of the real one. Regtest and real nodes reject each other's
    /// blocks.
    #[arg(long)]
    regtest: bool,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let cpu_miner = if cli.mine {
        let miner_hex = cli.miner.expect("--mine requires --miner <hex address>");
        let bytes = hex::decode(&miner_hex).expect("--miner must be hex");
        Some(bytes.try_into().expect("--miner must be 32 bytes"))
    } else {
        if cli.miner.is_some() {
            eprintln!("--miner has no effect without --mine");
        }
        None
    };
    let config = ServiceConfig {
        rest_addr: cli.rest_addr,
        p2p_addr: cli.p2p_addr,
        peers: cli.peers,
        no_listen: cli.no_listen,
        advertise: cli.advertise,
        max_peers: cli.max_peers,
        relay: cli.relay,
        key_file: cli.key_file,
        cpu_miner,
        data_dir: cli.data_dir,
        regtest: cli.regtest,
    };
    if let Err(e) = run(config).await {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
