//! The peer-link seam.
//!
//! A peer is a bidirectional byte stream, nothing more. All protocol logic runs
//! over a `PeerLink` and never learns whether that stream is a direct TCP
//! connection or a channel through a relay. A `tokio::net::TcpStream` is a
//! `PeerLink` today; a relay channel will implement the same traits later, so
//! node logic does not change when the relay is added.

use tokio::io::{AsyncRead, AsyncWrite};

/// Marker for anything usable as a peer connection: a readable, writable,
/// movable byte stream that can be sent across tasks.
pub trait PeerLink: AsyncRead + AsyncWrite + Unpin + Send {}

impl<T: AsyncRead + AsyncWrite + Unpin + Send> PeerLink for T {}
