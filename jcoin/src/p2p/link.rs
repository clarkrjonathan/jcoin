//! The peer-link seam.
//!
//! A peer is a bidirectional byte stream, nothing more. All protocol logic runs
//! over a `PeerLink` and never learns what carries it. In the node it is a
//! libp2p stream (over TCP or a relay circuit, encrypted with Noise); in tests
//! it is also a plain `tokio::net::TcpStream` or an in-memory duplex.

use tokio::io::{AsyncRead, AsyncWrite};

/// Marker for anything usable as a peer connection: a readable, writable,
/// movable byte stream that can be sent across tasks.
pub trait PeerLink: AsyncRead + AsyncWrite + Unpin + Send {}

impl<T: AsyncRead + AsyncWrite + Unpin + Send> PeerLink for T {}
