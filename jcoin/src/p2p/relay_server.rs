//! The relay router: pairs channels by directory handle and forwards opaque
//! `DATA` between paired nodes. It never parses the `JEFY` bytes it carries.
//!
//! A single router task owns all cross-connection state; per-connection reader
//! and writer tasks talk to it over channels.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

use super::addr::HANDLE_LEN;
use super::relay::{kind, read_relay_frame, write_relay_frame, RelayFrame};

type ConnId = u64;

struct Conn {
    out: mpsc::Sender<RelayFrame>,
    next_odd: u32,
    /// local channel id -> (peer connection, peer's channel id)
    channels: HashMap<u32, (ConnId, u32)>,
}

enum Event {
    New {
        id: ConnId,
        out: mpsc::Sender<RelayFrame>,
    },
    Frame {
        from: ConnId,
        frame: RelayFrame,
    },
    Closed {
        id: ConnId,
    },
}

/// Accept connections on `listener` and route forever.
pub async fn serve(listener: TcpListener) {
    let (events_tx, events_rx) = mpsc::channel::<Event>(1024);
    tokio::spawn(router(events_rx));

    let ids = Arc::new(AtomicU64::new(0));
    loop {
        match listener.accept().await {
            Ok((stream, addr)) => {
                let id = ids.fetch_add(1, Ordering::Relaxed);
                println!("[relay] connection {id} from {addr}");
                tokio::spawn(serve_conn(id, stream, events_tx.clone()));
            }
            Err(e) => eprintln!("[relay] accept error: {e}"),
        }
    }
}

async fn serve_conn(id: ConnId, stream: TcpStream, events: mpsc::Sender<Event>) {
    let (mut read_half, mut write_half) = stream.into_split();

    let (out_tx, mut out_rx) = mpsc::channel::<RelayFrame>(256);
    let writer = tokio::spawn(async move {
        while let Some(frame) = out_rx.recv().await {
            if write_relay_frame(&mut write_half, frame.channel, frame.kind, &frame.payload)
                .await
                .is_err()
            {
                break;
            }
        }
    });

    if events.send(Event::New { id, out: out_tx }).await.is_err() {
        return;
    }

    loop {
        match read_relay_frame(&mut read_half).await {
            Ok(frame) => {
                if events.send(Event::Frame { from: id, frame }).await.is_err() {
                    break;
                }
            }
            Err(_) => break,
        }
    }

    let _ = events.send(Event::Closed { id }).await;
    writer.abort();
}

async fn router(mut events: mpsc::Receiver<Event>) {
    let mut conns: HashMap<ConnId, Conn> = HashMap::new();
    let mut directory: HashMap<Vec<u8>, ConnId> = HashMap::new();

    while let Some(event) = events.recv().await {
        match event {
            Event::New { id, out } => {
                conns.insert(
                    id,
                    Conn {
                        out,
                        next_odd: 1,
                        channels: HashMap::new(),
                    },
                );
            }

            Event::Frame { from, frame } => match frame.kind {
                kind::REGISTER => {
                    let handle: [u8; HANDLE_LEN] = rand::random();
                    directory.insert(handle.to_vec(), from);
                    if let Some(conn) = conns.get(&from) {
                        let _ = conn.out.try_send(RelayFrame {
                            channel: 0,
                            kind: kind::REGISTERED,
                            payload: handle.to_vec(),
                        });
                    }
                    println!("[relay] conn {from} registered handle {}", hex::encode(handle));
                }

                kind::OPEN => {
                    let chan_a = frame.channel;
                    let target = frame.payload;
                    let fail = |conns: &HashMap<ConnId, Conn>, reason: &str| {
                        if let Some(conn) = conns.get(&from) {
                            let _ = conn.out.try_send(RelayFrame {
                                channel: chan_a,
                                kind: kind::ERROR,
                                payload: reason.as_bytes().to_vec(),
                            });
                        }
                    };

                    let Some(&conn_b) = directory.get(&target) else {
                        fail(&conns, "no such handle");
                        continue;
                    };
                    if conn_b == from {
                        fail(&conns, "cannot open a channel to self");
                        continue;
                    }

                    let chan_b = {
                        let b = conns.get_mut(&conn_b).unwrap();
                        let id = b.next_odd;
                        b.next_odd += 2;
                        b.channels.insert(id, (from, chan_a));
                        id
                    };
                    conns
                        .get_mut(&from)
                        .unwrap()
                        .channels
                        .insert(chan_a, (conn_b, chan_b));

                    let out_a = conns[&from].out.clone();
                    let out_b = conns[&conn_b].out.clone();
                    let _ = out_a.try_send(RelayFrame {
                        channel: chan_a,
                        kind: kind::PAIRED,
                        payload: Vec::new(),
                    });
                    let _ = out_b.try_send(RelayFrame {
                        channel: chan_b,
                        kind: kind::INCOMING,
                        payload: Vec::new(),
                    });
                }

                kind::DATA | kind::CLOSE => {
                    let mapping = conns
                        .get(&from)
                        .and_then(|c| c.channels.get(&frame.channel).copied());
                    if let Some((peer, peer_chan)) = mapping {
                        if let Some(peer_conn) = conns.get(&peer) {
                            let _ = peer_conn.out.try_send(RelayFrame {
                                channel: peer_chan,
                                kind: frame.kind,
                                payload: frame.payload,
                            });
                        }
                        if frame.kind == kind::CLOSE {
                            if let Some(c) = conns.get_mut(&from) {
                                c.channels.remove(&frame.channel);
                            }
                            if let Some(c) = conns.get_mut(&peer) {
                                c.channels.remove(&peer_chan);
                            }
                        }
                    }
                }

                _ => { /* nodes send only REGISTER/OPEN/DATA/CLOSE; ignore the rest */ }
            },

            Event::Closed { id } => {
                if let Some(conn) = conns.remove(&id) {
                    for (_local, (peer, peer_chan)) in conn.channels {
                        if let Some(peer_conn) = conns.get_mut(&peer) {
                            let _ = peer_conn.out.try_send(RelayFrame {
                                channel: peer_chan,
                                kind: kind::CLOSE,
                                payload: Vec::new(),
                            });
                            peer_conn.channels.remove(&peer_chan);
                        }
                    }
                }
                directory.retain(|_handle, owner| *owner != id);
                println!("[relay] connection {id} closed");
            }
        }
    }
}
