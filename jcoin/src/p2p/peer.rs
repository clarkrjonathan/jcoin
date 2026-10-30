//! Per-connection peer logic: the handshake, and (added next) the run loop.

use tokio::io::{AsyncRead, AsyncWrite};

use super::frame::PROTOCOL_VERSION;
use super::message::{read_message, write_message, Hello, Message, MessageError};

/// Lowest wire version this node will talk to.
pub const MIN_PROTOCOL_VERSION: u16 = 1;

/// Errors from the handshake.
#[derive(Debug)]
pub enum HandshakeError {
    Message(MessageError),
    IncompatibleVersion(u16),
    SelfConnection,
    Unexpected(&'static str),
}

impl std::fmt::Display for HandshakeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HandshakeError::Message(e) => write!(f, "{e}"),
            HandshakeError::IncompatibleVersion(v) => {
                write!(f, "peer wire version {v} is not supported")
            }
            HandshakeError::SelfConnection => write!(f, "connected to self (nonce match)"),
            HandshakeError::Unexpected(what) => write!(f, "expected {what} during handshake"),
        }
    }
}

impl std::error::Error for HandshakeError {}

impl From<MessageError> for HandshakeError {
    fn from(e: MessageError) -> Self {
        HandshakeError::Message(e)
    }
}

/// Build this node's `HELLO` for a new connection. The `nonce` must be random
/// per connection; it is what detects a self-connection.
pub fn local_hello(nonce: u64, listen_port: u16) -> Hello {
    let timestamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    Hello {
        version: PROTOCOL_VERSION,
        nonce,
        timestamp,
        listen_port,
        services: 0,
    }
}

/// Run the symmetric handshake over an established link. Both sides call this.
///
/// Sequence: send `HELLO`, read the peer's `HELLO` and validate it, send
/// `HELLO_ACK`, read the peer's `HELLO_ACK`. On success, return the peer's
/// `HELLO`. The steps are strictly ordered, so no concurrency is needed here;
/// the link is split for the run loop only after this returns.
pub async fn handshake<L>(link: &mut L, local: &Hello) -> Result<Hello, HandshakeError>
where
    L: AsyncRead + AsyncWrite + Unpin,
{
    write_message(link, &Message::Hello(local.clone()))
        .await
        .map_err(MessageError::from)?;

    let peer = match read_message(link).await? {
        Message::Hello(h) => h,
        _ => return Err(HandshakeError::Unexpected("HELLO")),
    };

    if peer.version < MIN_PROTOCOL_VERSION {
        return Err(HandshakeError::IncompatibleVersion(peer.version));
    }
    if peer.nonce == local.nonce {
        return Err(HandshakeError::SelfConnection);
    }

    write_message(link, &Message::HelloAck)
        .await
        .map_err(MessageError::from)?;

    match read_message(link).await? {
        Message::HelloAck => {}
        _ => return Err(HandshakeError::Unexpected("HELLO_ACK")),
    }

    Ok(peer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn two_peers_handshake() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        let hello_a = local_hello(1, 8333);
        let hello_b = local_hello(2, 8334);

        let ha = hello_a.clone();
        let hb = hello_b.clone();
        let task_a = tokio::spawn(async move { handshake(&mut a, &ha).await.map(|h| h.nonce) });
        let task_b = tokio::spawn(async move { handshake(&mut b, &hb).await.map(|h| h.nonce) });

        // Each side learns the other's nonce.
        assert_eq!(task_a.await.unwrap().unwrap(), hello_b.nonce);
        assert_eq!(task_b.await.unwrap().unwrap(), hello_a.nonce);
    }

    #[tokio::test]
    async fn self_connection_is_detected() {
        let (mut a, mut b) = tokio::io::duplex(1024);
        let same = local_hello(42, 8333);

        let s1 = same.clone();
        let s2 = same.clone();
        let task_a = tokio::spawn(async move { handshake(&mut a, &s1).await });
        let task_b = tokio::spawn(async move { handshake(&mut b, &s2).await });

        // At least one side must see the nonce clash and reject.
        let ra = task_a.await.unwrap();
        let rb = task_b.await.unwrap();
        assert!(
            matches!(ra, Err(HandshakeError::SelfConnection))
                || matches!(rb, Err(HandshakeError::SelfConnection))
        );
    }
}
