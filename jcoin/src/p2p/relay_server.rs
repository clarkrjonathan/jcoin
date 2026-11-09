//! The relay router: pairs channels by directory handle and forwards opaque
//! `DATA` between paired nodes. It never parses the `JEFY` bytes it carries.
//!
//! A single router task owns all cross-connection state; per-connection reader
//! and writer tasks talk to it over channels.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, oneshot};

use super::addr::HANDLE_LEN;
use super::relay::{kind, read_relay_frame, write_relay_frame, RelayFrame};

type ConnId = u64;

/// Most live channels a single connection may hold (abuse limit).
const MAX_CHANNELS_PER_CONN: usize = 64;

/// Frames queued toward one connection before it counts as too slow. A
/// connection whose queue is full is evicted (see `Router::send`).
const CONN_QUEUE: usize = 256;

struct Conn {
    out: mpsc::Sender<RelayFrame>,
    /// Fired to make `serve_conn` close the TCP connection.
    kill: Option<oneshot::Sender<()>>,
    next_odd: u32,
    /// local channel id -> (peer connection, peer's channel id)
    channels: HashMap<u32, (ConnId, u32)>,
}

enum Event {
    New {
        id: ConnId,
        out: mpsc::Sender<RelayFrame>,
        kill: oneshot::Sender<()>,
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

    let (out_tx, mut out_rx) = mpsc::channel::<RelayFrame>(CONN_QUEUE);
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

    let (kill_tx, mut kill_rx) = oneshot::channel();
    if events
        .send(Event::New {
            id,
            out: out_tx,
            kill: kill_tx,
        })
        .await
        .is_err()
    {
        return;
    }

    loop {
        tokio::select! {
            read = read_relay_frame(&mut read_half) => match read {
                Ok(frame) => {
                    if events.send(Event::Frame { from: id, frame }).await.is_err() {
                        break;
                    }
                }
                Err(_) => break,
            },
            // The router evicted this connection. Dropping both halves closes
            // the TCP connection, so the node sees the loss and reconnects.
            _ = &mut kill_rx => break,
        }
    }

    let _ = events.send(Event::Closed { id }).await;
    writer.abort();
}

/// All cross-connection state, owned by the router task.
struct Router {
    conns: HashMap<ConnId, Conn>,
    directory: HashMap<Vec<u8>, ConnId>,
    /// Connections to evict once the current event is handled.
    doomed: Vec<ConnId>,
}

impl Router {
    /// Queue a frame toward `to` without blocking the router. The relay
    /// promises each channel a reliable byte stream, so a frame is never
    /// silently dropped: if `to` cannot take it (queue full or gone), the
    /// whole connection `to` is evicted and its peers get `CLOSE`. A stream is
    /// then complete or explicitly closed, never missing bytes in the middle.
    fn send(&mut self, to: ConnId, frame: RelayFrame) {
        let Some(conn) = self.conns.get(&to) else {
            return;
        };
        if conn.out.try_send(frame).is_err() && !self.doomed.contains(&to) {
            eprintln!("[relay] connection {to} cannot keep up; evicting it");
            self.doomed.push(to);
        }
    }

    /// Remove a connection: notify the peer side of each of its channels with
    /// `CLOSE`, drop it from the directory, and (if `kill`) close its TCP
    /// connection. A peer that cannot take its `CLOSE` is evicted in turn.
    fn remove(&mut self, id: ConnId, kill: bool) {
        if let Some(mut conn) = self.conns.remove(&id) {
            for (_local, (peer, peer_chan)) in conn.channels.drain() {
                if let Some(peer_conn) = self.conns.get_mut(&peer) {
                    peer_conn.channels.remove(&peer_chan);
                }
                self.send(
                    peer,
                    RelayFrame {
                        channel: peer_chan,
                        kind: kind::CLOSE,
                        payload: Vec::new(),
                    },
                );
            }
            if kill {
                if let Some(k) = conn.kill.take() {
                    let _ = k.send(());
                }
            }
        }
        self.directory.retain(|_handle, owner| *owner != id);
    }

    /// Evict every connection marked during the last event, including any
    /// that become doomed while their peers are notified.
    fn evict_doomed(&mut self) {
        while let Some(id) = self.doomed.pop() {
            self.remove(id, true);
        }
    }

    fn handle_frame(&mut self, from: ConnId, frame: RelayFrame) {
        match frame.kind {
            kind::REGISTER => {
                // One handle per connection: ignore repeat registrations so a
                // single peer cannot flood the directory.
                if self.directory.values().any(|&owner| owner == from) {
                    return;
                }
                let handle: [u8; HANDLE_LEN] = rand::random();
                self.directory.insert(handle.to_vec(), from);
                self.send(
                    from,
                    RelayFrame {
                        channel: 0,
                        kind: kind::REGISTERED,
                        payload: handle.to_vec(),
                    },
                );
                println!("[relay] conn {from} registered handle {}", hex::encode(handle));
            }

            kind::LIST => {
                // Return the handles of every other registered node, so a node
                // can discover peers to open channels to without knowing
                // handles out of band.
                let mut payload = Vec::new();
                for (handle, &owner) in &self.directory {
                    if owner != from {
                        payload.extend_from_slice(handle);
                    }
                }
                self.send(
                    from,
                    RelayFrame {
                        channel: 0,
                        kind: kind::HANDLES,
                        payload,
                    },
                );
            }

            kind::OPEN => {
                let chan_a = frame.channel;
                let target = frame.payload;
                let fail = |router: &mut Router, reason: &str| {
                    router.send(
                        from,
                        RelayFrame {
                            channel: chan_a,
                            kind: kind::ERROR,
                            payload: reason.as_bytes().to_vec(),
                        },
                    );
                };

                let Some(&conn_b) = self.directory.get(&target) else {
                    fail(self, "no such handle");
                    return;
                };
                if conn_b == from {
                    fail(self, "cannot open a channel to self");
                    return;
                }
                if !self.conns.contains_key(&from) || !self.conns.contains_key(&conn_b) {
                    return;
                }
                // Abuse limit: cap live channels on either side.
                let over_cap = self.conns[&from].channels.len() >= MAX_CHANNELS_PER_CONN
                    || self.conns[&conn_b].channels.len() >= MAX_CHANNELS_PER_CONN;
                if over_cap {
                    fail(self, "channel limit reached");
                    return;
                }

                let chan_b = {
                    let b = self.conns.get_mut(&conn_b).unwrap();
                    let id = b.next_odd;
                    b.next_odd += 2;
                    b.channels.insert(id, (from, chan_a));
                    id
                };
                self.conns
                    .get_mut(&from)
                    .unwrap()
                    .channels
                    .insert(chan_a, (conn_b, chan_b));

                self.send(
                    from,
                    RelayFrame {
                        channel: chan_a,
                        kind: kind::PAIRED,
                        payload: Vec::new(),
                    },
                );
                self.send(
                    conn_b,
                    RelayFrame {
                        channel: chan_b,
                        kind: kind::INCOMING,
                        payload: Vec::new(),
                    },
                );
            }

            kind::DATA | kind::CLOSE => {
                let mapping = self
                    .conns
                    .get(&from)
                    .and_then(|c| c.channels.get(&frame.channel).copied());
                if let Some((peer, peer_chan)) = mapping {
                    if frame.kind == kind::CLOSE {
                        if let Some(c) = self.conns.get_mut(&from) {
                            c.channels.remove(&frame.channel);
                        }
                        if let Some(c) = self.conns.get_mut(&peer) {
                            c.channels.remove(&peer_chan);
                        }
                    }
                    self.send(
                        peer,
                        RelayFrame {
                            channel: peer_chan,
                            kind: frame.kind,
                            payload: frame.payload,
                        },
                    );
                }
            }

            _ => { /* nodes send REGISTER, LIST, OPEN, DATA, CLOSE; ignore the rest */ }
        }
    }
}

async fn router(mut events: mpsc::Receiver<Event>) {
    let mut router = Router {
        conns: HashMap::new(),
        directory: HashMap::new(),
        doomed: Vec::new(),
    };

    while let Some(event) = events.recv().await {
        match event {
            Event::New { id, out, kill } => {
                router.conns.insert(
                    id,
                    Conn {
                        out,
                        kill: Some(kill),
                        next_odd: 1,
                        channels: HashMap::new(),
                    },
                );
            }
            Event::Frame { from, frame } => router.handle_frame(from, frame),
            Event::Closed { id } => {
                router.remove(id, false);
                println!("[relay] connection {id} closed");
            }
        }
        router.evict_doomed();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    async fn start_relay() -> std::net::SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(serve(listener));
        addr
    }

    // S6: a receiver that stops reading must not get a stream with bytes
    // silently missing. The router evicts it, and the sender sees CLOSE.
    #[tokio::test]
    async fn slow_receiver_is_evicted_and_sender_sees_close() {
        let relay = start_relay().await;

        // B registers, then never reads again.
        let mut b = TcpStream::connect(relay).await.unwrap();
        write_relay_frame(&mut b, 0, kind::REGISTER, &[]).await.unwrap();
        let registered = read_relay_frame(&mut b).await.unwrap();
        assert_eq!(registered.kind, kind::REGISTERED);
        let handle = registered.payload;

        // A opens a channel to B.
        let a = TcpStream::connect(relay).await.unwrap();
        let (mut a_read, mut a_write) = a.into_split();
        const CHAN: u32 = 2;
        write_relay_frame(&mut a_write, CHAN, kind::OPEN, &handle).await.unwrap();
        let paired = read_relay_frame(&mut a_read).await.unwrap();
        assert_eq!((paired.channel, paired.kind), (CHAN, kind::PAIRED));

        // Flood B through the channel: far more than B's socket buffers plus
        // its router queue can hold.
        let flood = tokio::spawn(async move {
            let chunk = vec![0xabu8; 64 * 1024];
            for _ in 0..2_000 {
                if write_relay_frame(&mut a_write, CHAN, kind::DATA, &chunk).await.is_err() {
                    break;
                }
            }
            a_write
        });

        let close = tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let frame = read_relay_frame(&mut a_read).await.unwrap();
                if frame.kind == kind::CLOSE {
                    return frame.channel;
                }
            }
        })
        .await
        .expect("sender never saw CLOSE: bytes were dropped instead");
        assert_eq!(close, CHAN);
        flood.abort();
        drop(b);
    }
}
