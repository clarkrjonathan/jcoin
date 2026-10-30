//! The `JEFY` frame envelope: a fixed 16-byte header followed by a payload.
//!
//! Header layout (big-endian), total 16 bytes:
//!   magic (4) | version (2) | msg_type (2) | payload_length (4) | checksum (4)
//! The checksum is the first 4 bytes of `SHA256(payload)`.

use sha2::{Digest, Sha256};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Network magic: ASCII "JEFY".
pub const MAGIC: [u8; 4] = *b"JEFY";

/// Current wire-protocol version. Distinct from the block/consensus version.
pub const PROTOCOL_VERSION: u16 = 1;

/// Fixed header size in bytes.
pub const HEADER_LEN: usize = 16;

/// Largest payload a node will accept. Caps allocation from a hostile peer.
/// Gossip-v1 messages are under 200 bytes; the headroom is for future blocks.
pub const MAX_PAYLOAD: u32 = 4 * 1024 * 1024;

/// Errors from reading or writing a frame.
#[derive(Debug)]
pub enum FrameError {
    Io(std::io::Error),
    BadMagic,
    TooLarge(u32),
    BadChecksum,
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FrameError::Io(e) => write!(f, "io error: {e}"),
            FrameError::BadMagic => write!(f, "frame did not start with the JEFY magic"),
            FrameError::TooLarge(n) => write!(f, "payload length {n} exceeds the cap"),
            FrameError::BadChecksum => write!(f, "payload checksum did not match"),
        }
    }
}

impl std::error::Error for FrameError {}

impl From<std::io::Error> for FrameError {
    fn from(e: std::io::Error) -> Self {
        FrameError::Io(e)
    }
}

/// First 4 bytes of `SHA256(payload)`.
pub fn checksum(payload: &[u8]) -> [u8; 4] {
    let mut hasher = Sha256::new();
    hasher.update(payload);
    let digest = hasher.finalize();
    [digest[0], digest[1], digest[2], digest[3]]
}

/// A validated frame with its message type left as a raw `u16`. Decoding the
/// payload into a `Message` is the job of the `message` module, so that an
/// unknown type is a decode concern rather than a transport error.
#[derive(Debug, Clone)]
pub struct RawFrame {
    pub version: u16,
    pub msg_type: u16,
    pub payload: Vec<u8>,
}

/// Write one frame: header then payload, then flush.
pub async fn write_frame<W: AsyncWrite + Unpin>(
    writer: &mut W,
    version: u16,
    msg_type: u16,
    payload: &[u8],
) -> Result<(), FrameError> {
    let mut header = [0u8; HEADER_LEN];
    header[0..4].copy_from_slice(&MAGIC);
    header[4..6].copy_from_slice(&version.to_be_bytes());
    header[6..8].copy_from_slice(&msg_type.to_be_bytes());
    header[8..12].copy_from_slice(&(payload.len() as u32).to_be_bytes());
    header[12..16].copy_from_slice(&checksum(payload));

    writer.write_all(&header).await?;
    writer.write_all(payload).await?;
    writer.flush().await?;
    Ok(())
}

/// Read one frame. Validates magic, payload cap, and checksum. A failure here
/// means the stream is unusable, so the caller should drop the connection.
pub async fn read_frame<R: AsyncRead + Unpin>(reader: &mut R) -> Result<RawFrame, FrameError> {
    let mut header = [0u8; HEADER_LEN];
    reader.read_exact(&mut header).await?;

    if header[0..4] != MAGIC {
        return Err(FrameError::BadMagic);
    }

    let version = u16::from_be_bytes([header[4], header[5]]);
    let msg_type = u16::from_be_bytes([header[6], header[7]]);
    let length = u32::from_be_bytes([header[8], header[9], header[10], header[11]]);
    let expected = [header[12], header[13], header[14], header[15]];

    if length > MAX_PAYLOAD {
        return Err(FrameError::TooLarge(length));
    }

    let mut payload = vec![0u8; length as usize];
    reader.read_exact(&mut payload).await?;

    if checksum(&payload) != expected {
        return Err(FrameError::BadChecksum);
    }

    Ok(RawFrame {
        version,
        msg_type,
        payload,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn frame_round_trips() {
        let payload = b"hello jeffy".to_vec();
        let mut buf: Vec<u8> = Vec::new();
        write_frame(&mut buf, 1, 0x0010, &payload).await.unwrap();

        assert_eq!(buf.len(), HEADER_LEN + payload.len());

        let frame = read_frame(&mut buf.as_slice()).await.unwrap();
        assert_eq!(frame.version, 1);
        assert_eq!(frame.msg_type, 0x0010);
        assert_eq!(frame.payload, payload);
    }

    #[tokio::test]
    async fn empty_payload_round_trips() {
        let mut buf: Vec<u8> = Vec::new();
        write_frame(&mut buf, 1, 0x0002, &[]).await.unwrap();
        let frame = read_frame(&mut buf.as_slice()).await.unwrap();
        assert_eq!(frame.msg_type, 0x0002);
        assert!(frame.payload.is_empty());
    }

    #[tokio::test]
    async fn bad_magic_is_rejected() {
        let mut buf: Vec<u8> = Vec::new();
        write_frame(&mut buf, 1, 0x0001, b"x").await.unwrap();
        buf[0] ^= 0xFF; // corrupt the magic
        let err = read_frame(&mut buf.as_slice()).await.unwrap_err();
        assert!(matches!(err, FrameError::BadMagic));
    }

    #[tokio::test]
    async fn bad_checksum_is_rejected() {
        let mut buf: Vec<u8> = Vec::new();
        write_frame(&mut buf, 1, 0x0001, b"payload").await.unwrap();
        let last = buf.len() - 1;
        buf[last] ^= 0xFF; // corrupt the payload after the checksum was computed
        let err = read_frame(&mut buf.as_slice()).await.unwrap_err();
        assert!(matches!(err, FrameError::BadChecksum));
    }
}
