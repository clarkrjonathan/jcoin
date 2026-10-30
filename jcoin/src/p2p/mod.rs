//! Peer-to-peer networking for JeffyCoin.
//!
//! See `docs/p2p-protocol.md` for the wire protocol. This module implements the
//! inner `JEFY` layer: the frame codec (`frame`), the message set (`message`),
//! and the peer-link seam (`link`). The outer relay transport is added later.

pub mod addr;
pub mod frame;
pub mod link;
pub mod message;
pub mod network;
pub mod peer;

pub use addr::{Endpoint, Handle, HANDLE_LEN};
pub use frame::{FrameError, RawFrame, HEADER_LEN, MAGIC, MAX_PAYLOAD, PROTOCOL_VERSION};
pub use link::PeerLink;
pub use message::{read_message, write_message, Hello, Message, MessageError};
pub use network::{Network, PeerId};
pub use peer::{handshake, local_hello, run_peer, HandshakeError, PEER_TIMEOUT, PING_INTERVAL};
