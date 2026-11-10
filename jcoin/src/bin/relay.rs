//! JeffyCoin relay binary: a libp2p circuit relay and rendezvous point for
//! nodes behind NAT (see `jcoin::p2p::relay_host`). It never parses the
//! JeffyCoin protocol, and the circuits it carries are encrypted end to end.
//!
//! Give nodes the address it prints at startup (`.../p2p/<relay peer id>`) as
//! their `--relay`. Keep `--key-file` stable so that the peer id, and so that
//! address, does not change across restarts.

use clap::Parser;

use jcoin::p2p::{load_or_create_keypair, parse_addr, run_relay};

#[derive(Parser)]
#[command(name = "jcoin-relay", about = "JeffyCoin relay (libp2p circuit relay + rendezvous)")]
struct Cli {
    /// Address to listen on: `ip:port` or a multiaddr.
    #[arg(long, default_value = "0.0.0.0:9000")]
    addr: String,
    /// The relay's public address (repeatable): `ip:port` or a multiaddr. The
    /// relay serves reservations only once it has one. Defaults to --addr when
    /// that is a concrete address.
    #[arg(long = "external")]
    external: Vec<String>,
    /// File holding the relay's identity key (created if missing).
    #[arg(long, default_value = "relay_key")]
    key_file: std::path::PathBuf,
    /// Print the address nodes should use as --relay (the first --external,
    /// or --addr, followed by /p2p/<relay peer id>) and exit.
    #[arg(long)]
    show_address: bool,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let listen = parse_addr(&cli.addr).expect("bad --addr");
    let mut external: Vec<_> = cli
        .external
        .iter()
        .map(|s| parse_addr(s).expect("bad --external"))
        .collect();
    let wildcard = cli.addr.starts_with("0.0.0.0") || cli.addr.starts_with("[::]");
    if external.is_empty() {
        if wildcard {
            eprintln!(
                "warning: --addr is a wildcard and no --external was given; \
                 the relay will not serve reservations until it knows a public address"
            );
        } else {
            external.push(listen.clone());
        }
    }
    let keypair = load_or_create_keypair(&cli.key_file).expect("cannot load or create the relay key");
    if cli.show_address {
        let base = external.first().cloned().unwrap_or_else(|| listen.clone());
        let id = keypair.public().to_peer_id();
        println!("{}", base.with(libp2p::multiaddr::Protocol::P2p(id)));
        return;
    }
    if let Err(e) = run_relay(keypair, vec![listen], external).await {
        eprintln!("relay stopped: {e}");
        std::process::exit(1);
    }
}
