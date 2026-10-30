//! The node-side relay client: one TCP connection to a relay, multiplexed into
//! per-peer channels.
//!
//! Each channel is bridged to an in-memory `DuplexStream`. The node side of that
//! duplex is handed to `run_peer`, which speaks plain `JEFY` and never knows a
//! relay is in the path. The relay side is split: the client writes inbound
//! `DATA` into it, and a forwarder reads what `run_peer` wrote and sends it back
//! as `DATA`. The relay only ever sees opaque `DATA` payloads.

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::time::Duration;

use tokio::io::{split, AsyncReadExt, AsyncWriteExt, DuplexStream, ReadHalf, WriteHalf};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

use super::addr::{Endpoint, Handle};
use super::network::Network;
use super::peer::run_peer;
use super::relay::{kind, read_relay_frame, write_relay_frame, RelayFrame};

/// In-memory buffer per channel duplex.
const DUPLEX_BUF: usize = 64 * 1024;
/// Read chunk when forwarding node output to the relay.
const FORWARD_CHUNK: usize = 8 * 1024;

/// Connect to a relay and keep the connection up, retrying on disconnect.
pub async fn run_relay_client(relay_addr: SocketAddr, network: Network) {
    loop {
        match TcpStream::connect(relay_addr).await {
            Ok(stream) => {
                println!("[relay-client] connected to relay {relay_addr}");
                serve(relay_addr, stream, network.clone()).await;
            }
            Err(e) => eprintln!("[relay-client] connect {relay_addr} failed: {e}"),
        }
        network.clear_relay_opener();
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
}

async fn serve(relay_addr: SocketAddr, stream: TcpStream, network: Network) {
    let (read_half, write_half) = stream.into_split();

    // Writer: drain outgoing relay frames to the socket.
    let (relay_tx, mut relay_rx) = mpsc::channel::<RelayFrame>(256);
    let mut wh = write_half;
    let writer = tokio::spawn(async move {
        while let Some(f) = relay_rx.recv().await {
            if write_relay_frame(&mut wh, f.channel, f.kind, &f.payload)
                .await
                .is_err()
            {
                break;
            }
        }
    });

    // Reader: forward decoded relay frames to the manager loop (never cancelled).
    let (frames_tx, mut frames_rx) = mpsc::channel::<RelayFrame>(256);
    let mut rh = read_half;
    let reader = tokio::spawn(async move {
        loop {
            match read_relay_frame(&mut rh).await {
                Ok(f) => {
                    if frames_tx.send(f).await.is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    // Learned Relay endpoints are opened by sending their handle here. A clone
    // stays local so registration can open handles already in the book.
    let (open_tx, mut open_rx) = mpsc::channel::<Handle>(64);
    network.set_relay_opener(open_tx.clone());

    // Register to become reachable.
    let _ = relay_tx
        .send(RelayFrame {
            channel: 0,
            kind: kind::REGISTER,
            payload: Vec::new(),
        })
        .await;

    // Manager state, owned by this one task (no locks).
    let mut table: HashMap<u32, WriteHalf<DuplexStream>> = HashMap::new();
    let mut pending: HashMap<u32, DuplexStream> = HashMap::new();
    let mut opened: HashSet<Handle> = HashSet::new();
    let mut next_even: u32 = 0;

    loop {
        tokio::select! {
            frame = frames_rx.recv() => {
                let Some(frame) = frame else { break };
                match frame.kind {
                    kind::REGISTERED => {
                        if let Ok(handle) = <Handle>::try_from(frame.payload.as_slice()) {
                            opened.insert(handle); // never open a channel to ourselves
                            network.add_advertised(Endpoint::Relay { relay: relay_addr, handle });
                            println!("[relay-client] registered, handle {}", hex::encode(handle));
                            // Open channels to any relay peers already known.
                            for peer_handle in network.relay_handles(relay_addr) {
                                let _ = open_tx.try_send(peer_handle);
                            }
                        }
                    }
                    kind::PAIRED => {
                        if let Some(node_side) = pending.remove(&frame.channel) {
                            tokio::spawn(run_peer(
                                node_side,
                                network.clone(),
                                format!("relay chan {} (out)", frame.channel),
                            ));
                        }
                    }
                    kind::INCOMING => {
                        let node_side = setup_channel(frame.channel, &mut table, &relay_tx);
                        tokio::spawn(run_peer(
                            node_side,
                            network.clone(),
                            format!("relay chan {} (in)", frame.channel),
                        ));
                    }
                    kind::DATA => {
                        if let Some(writer) = table.get_mut(&frame.channel) {
                            if writer.write_all(&frame.payload).await.is_err() {
                                table.remove(&frame.channel);
                            }
                        }
                    }
                    kind::CLOSE => {
                        table.remove(&frame.channel);
                        pending.remove(&frame.channel);
                    }
                    kind::ERROR => {
                        eprintln!(
                            "[relay-client] open on chan {} failed: {}",
                            frame.channel,
                            String::from_utf8_lossy(&frame.payload)
                        );
                        table.remove(&frame.channel);
                        pending.remove(&frame.channel);
                    }
                    _ => {}
                }
            }
            want = open_rx.recv() => {
                let Some(handle) = want else { continue };
                if !opened.insert(handle) {
                    continue; // already opened or is our own handle
                }
                let chan = next_even;
                next_even = next_even.wrapping_add(2);
                let node_side = setup_channel(chan, &mut table, &relay_tx);
                pending.insert(chan, node_side);
                let _ = relay_tx
                    .send(RelayFrame {
                        channel: chan,
                        kind: kind::OPEN,
                        payload: handle.to_vec(),
                    })
                    .await;
            }
        }
    }

    network.clear_relay_opener();
    reader.abort();
    writer.abort();
}

/// Create a channel's duplex, store the relay-side writer, spawn the forwarder,
/// and return the node side for `run_peer`.
fn setup_channel(
    chan: u32,
    table: &mut HashMap<u32, WriteHalf<DuplexStream>>,
    relay_tx: &mpsc::Sender<RelayFrame>,
) -> DuplexStream {
    let (node_side, relay_side) = tokio::io::duplex(DUPLEX_BUF);
    let (relay_read, relay_write) = split(relay_side);
    table.insert(chan, relay_write);
    spawn_forwarder(chan, relay_read, relay_tx.clone());
    node_side
}

/// Read what `run_peer` wrote on this channel and send it to the relay as
/// `DATA`; on end of stream, send `CLOSE`.
fn spawn_forwarder(chan: u32, mut relay_read: ReadHalf<DuplexStream>, relay_tx: mpsc::Sender<RelayFrame>) {
    tokio::spawn(async move {
        let mut buf = [0u8; FORWARD_CHUNK];
        loop {
            match relay_read.read(&mut buf).await {
                Ok(0) => break,
                Ok(n) => {
                    let frame = RelayFrame {
                        channel: chan,
                        kind: kind::DATA,
                        payload: buf[..n].to_vec(),
                    };
                    if relay_tx.send(frame).await.is_err() {
                        return;
                    }
                }
                Err(_) => break,
            }
        }
        let _ = relay_tx
            .send(RelayFrame {
                channel: chan,
                kind: kind::CLOSE,
                payload: Vec::new(),
            })
            .await;
    });
}
