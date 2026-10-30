//! Peer-to-peer networking for JeffyCoin.
//!
//! See `docs/p2p-protocol.md` for the wire protocol. This module implements the
//! inner `JEFY` layer: the frame codec (`frame`), the message set (`message`),
//! and the peer-link seam (`link`). The outer relay transport is added later.

pub mod frame;
pub mod link;
pub mod message;

pub use frame::{FrameError, RawFrame, HEADER_LEN, MAGIC, MAX_PAYLOAD, PROTOCOL_VERSION};
pub use link::PeerLink;
pub use message::{read_message, write_message, Hello, Message, MessageError};
