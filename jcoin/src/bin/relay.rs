//! JeffyCoin relay binary: a thin wrapper around the relay router in the
//! library (`jcoin::p2p::relay_server`). It pairs node channels by directory
//! handle and forwards opaque `DATA`, never parsing the `JEFY` bytes inside.

use clap::Parser;
use tokio::net::TcpListener;

use jcoin::p2p::relay_server::serve;

#[derive(Parser)]
#[command(name = "jcoin-relay", about = "JeffyCoin relay (multiplexing router)")]
struct Cli {
    /// Address to listen on.
    #[arg(long, default_value = "0.0.0.0:9000")]
    addr: String,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let listener = TcpListener::bind(&cli.addr)
        .await
        .expect("failed to bind relay address");
    println!("JeffyCoin relay listening on {}", cli.addr);
    serve(listener).await;
}
