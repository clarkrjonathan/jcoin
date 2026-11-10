//! Peer-to-peer networking for JeffyCoin.
//!
//! See `docs/p2p-protocol.md` for the wire protocol. Two layers:
//! - the JeffyCoin protocol: the frame codec (`frame`), the message set
//!   (`message`), addresses (`addr`), per-peer sessions (`peer`), and the
//!   shared peer registry and address book (`network`). It runs over any
//!   `PeerLink` (the seam in `link`).
//! - the libp2p transport (`host`): identity, Noise encryption, Yamux, relay
//!   circuits, hole punching, and rendezvous discovery. Each JeffyCoin session
//!   runs on one libp2p stream. `relay_host` is the relay and rendezvous
//!   server.

pub mod addr;
pub mod frame;
pub mod host;
pub mod link;
pub mod message;
pub mod network;
pub mod peer;
pub mod relay_host;

pub use frame::{FrameError, RawFrame, HEADER_LEN, MAGIC, MAX_PAYLOAD, PROTOCOL_VERSION};
pub use host::{load_or_create_keypair, parse_addr, run_host, HostConfig, PROTOCOL};
pub use link::PeerLink;
pub use message::{read_message, write_message, Hello, Message, MessageError};
pub use network::{Network, PeerId};
pub use peer::{handshake, local_hello, run_peer, HandshakeError, PEER_TIMEOUT, PING_INTERVAL};
pub use relay_host::run_relay;
