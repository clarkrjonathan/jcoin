//! The outer relay transport: framing and control messages for the multiplexing
//! router.
//!
//! One TCP connection between a node and the relay carries many channels, each a
//! logical peer link. A frame is a 9-byte header followed by a payload:
//!   channel (4) | kind (1) | payload_length (4)
//! `DATA` payloads are opaque `JEFY` bytes the relay never parses.
//!
//! Channel id ownership on one node-relay connection is split to avoid clashes:
//! the node allocates even ids for channels it opens; the relay allocates odd
//! ids for channels it pushes to the node (`INCOMING`).

use std::io;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Relay frame header size.
pub const RELAY_HEADER_LEN: usize = 9;

/// Largest relay payload accepted.
pub const MAX_RELAY_PAYLOAD: u32 = 4 * 1024 * 1024;

/// Control and data frame kinds.
pub mod kind {
    /// node -> relay: register to be reachable. Payload empty.
    pub const REGISTER: u8 = 0x01;
    /// relay -> node: assigned directory handle. Payload = handle bytes.
    pub const REGISTERED: u8 = 0x02;
    /// node -> relay: open a channel to a target handle. `channel` = opener's
    /// local id (even); payload = target handle.
    pub const OPEN: u8 = 0x03;
    /// relay -> node: a peer opened a channel to this node. `channel` = the new
    /// local id (odd). Payload empty.
    pub const INCOMING: u8 = 0x04;
    /// relay -> node: an opened channel is now live. `channel` = the opener's id.
    pub const PAIRED: u8 = 0x05;
    /// both: opaque channel data. Payload = JEFY bytes.
    pub const DATA: u8 = 0x06;
    /// both: close a channel. Payload empty.
    pub const CLOSE: u8 = 0x07;
    /// relay -> node: an OPEN failed. `channel` = opener's id; payload = reason.
    pub const ERROR: u8 = 0x08;
}

/// One relay frame.
#[derive(Debug, Clone)]
pub struct RelayFrame {
    pub channel: u32,
    pub kind: u8,
    pub payload: Vec<u8>,
}

/// Write one relay frame and flush.
pub async fn write_relay_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    channel: u32,
    kind: u8,
    payload: &[u8],
) -> io::Result<()> {
    let mut header = [0u8; RELAY_HEADER_LEN];
    header[0..4].copy_from_slice(&channel.to_be_bytes());
    header[4] = kind;
    header[5..9].copy_from_slice(&(payload.len() as u32).to_be_bytes());
    writer.write_all(&header).await?;
    writer.write_all(payload).await?;
    writer.flush().await?;
    Ok(())
}

/// Read one relay frame.
pub async fn read_relay_frame<R: AsyncRead + Unpin>(reader: &mut R) -> io::Result<RelayFrame> {
    let mut header = [0u8; RELAY_HEADER_LEN];
    reader.read_exact(&mut header).await?;

    let channel = u32::from_be_bytes([header[0], header[1], header[2], header[3]]);
    let kind = header[4];
    let length = u32::from_be_bytes([header[5], header[6], header[7], header[8]]);

    if length > MAX_RELAY_PAYLOAD {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "relay payload exceeds the cap",
        ));
    }

    let mut payload = vec![0u8; length as usize];
    reader.read_exact(&mut payload).await?;

    Ok(RelayFrame {
        channel,
        kind,
        payload,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn relay_frame_round_trips() {
        let mut buf: Vec<u8> = Vec::new();
        write_relay_frame(&mut buf, 4, kind::DATA, b"opaque jefy bytes")
            .await
            .unwrap();

        let frame = read_relay_frame(&mut buf.as_slice()).await.unwrap();
        assert_eq!(frame.channel, 4);
        assert_eq!(frame.kind, kind::DATA);
        assert_eq!(frame.payload, b"opaque jefy bytes");
    }

    #[tokio::test]
    async fn empty_payload_round_trips() {
        let mut buf: Vec<u8> = Vec::new();
        write_relay_frame(&mut buf, 1, kind::REGISTER, &[])
            .await
            .unwrap();
        let frame = read_relay_frame(&mut buf.as_slice()).await.unwrap();
        assert_eq!(frame.kind, kind::REGISTER);
        assert!(frame.payload.is_empty());
    }
}
