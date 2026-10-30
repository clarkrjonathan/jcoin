//! The gossip-v1 message set and its payload encoding.
//!
//! Implemented here: `HELLO`, `HELLO_ACK`, `PING`, `PONG`, `NEW_TRANSACTION`.
//! Peer-address exchange (`GET_ADDR` / `ADDR`) is added with that milestone.

use tokio::io::{AsyncRead, AsyncWrite};

use crate::Transaction;

use super::addr::{decode_addrs, encode_addrs, Endpoint};
use super::frame::{read_frame, write_frame, FrameError, RawFrame};

/// Message type codes, matching `docs/p2p-protocol.md`.
pub mod type_code {
    pub const HELLO: u16 = 0x0001;
    pub const HELLO_ACK: u16 = 0x0002;
    pub const PING: u16 = 0x0003;
    pub const PONG: u16 = 0x0004;
    pub const NEW_TRANSACTION: u16 = 0x0010;
    pub const GET_ADDR: u16 = 0x0020;
    pub const ADDR: u16 = 0x0021;
}

/// Payload of a `HELLO`. Carries no persistent node identifier: the `nonce` is
/// per-connection and only detects self-connections and duplicate links.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hello {
    pub version: u16,
    pub nonce: u64,
    pub timestamp: u64,
    pub listen_port: u16,
    pub services: u64,
}

impl Hello {
    /// Fixed encoded size: 2 + 8 + 8 + 2 + 8.
    pub const ENCODED_LEN: usize = 28;

    fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(Self::ENCODED_LEN);
        bytes.extend_from_slice(&self.version.to_be_bytes());
        bytes.extend_from_slice(&self.nonce.to_be_bytes());
        bytes.extend_from_slice(&self.timestamp.to_be_bytes());
        bytes.extend_from_slice(&self.listen_port.to_be_bytes());
        bytes.extend_from_slice(&self.services.to_be_bytes());
        bytes
    }

    fn decode(payload: &[u8]) -> Option<Hello> {
        if payload.len() != Self::ENCODED_LEN {
            return None;
        }
        Some(Hello {
            version: u16::from_be_bytes(payload[0..2].try_into().ok()?),
            nonce: u64::from_be_bytes(payload[2..10].try_into().ok()?),
            timestamp: u64::from_be_bytes(payload[10..18].try_into().ok()?),
            listen_port: u16::from_be_bytes(payload[18..20].try_into().ok()?),
            services: u64::from_be_bytes(payload[20..28].try_into().ok()?),
        })
    }
}

/// A decoded gossip-v1 message.
#[derive(Debug, Clone)]
pub enum Message {
    Hello(Hello),
    HelloAck,
    Ping(u64),
    Pong(u64),
    NewTransaction(Transaction),
    /// Request the peer's known addresses.
    GetAddr,
    /// A list of peer addresses.
    Addr(Vec<Endpoint>),
}

/// Errors from reading or decoding a message.
#[derive(Debug)]
pub enum MessageError {
    Frame(FrameError),
    /// A message type this node does not know. The caller may ignore it and
    /// continue, since unknown types are not a transport failure.
    UnknownType(u16),
    BadLength { msg_type: u16, len: usize },
    BadTransaction,
    BadAddrs,
}

impl std::fmt::Display for MessageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MessageError::Frame(e) => write!(f, "{e}"),
            MessageError::UnknownType(t) => write!(f, "unknown message type {t:#06x}"),
            MessageError::BadLength { msg_type, len } => {
                write!(f, "wrong payload length {len} for message type {msg_type:#06x}")
            }
            MessageError::BadTransaction => write!(f, "payload was not a valid transaction"),
            MessageError::BadAddrs => write!(f, "payload was not a valid address list"),
        }
    }
}

impl std::error::Error for MessageError {}

impl From<FrameError> for MessageError {
    fn from(e: FrameError) -> Self {
        MessageError::Frame(e)
    }
}

impl Message {
    /// The wire type code for this message.
    pub fn type_code(&self) -> u16 {
        match self {
            Message::Hello(_) => type_code::HELLO,
            Message::HelloAck => type_code::HELLO_ACK,
            Message::Ping(_) => type_code::PING,
            Message::Pong(_) => type_code::PONG,
            Message::NewTransaction(_) => type_code::NEW_TRANSACTION,
            Message::GetAddr => type_code::GET_ADDR,
            Message::Addr(_) => type_code::ADDR,
        }
    }

    /// Encode just the payload (the frame header is added by `write_message`).
    pub fn encode_payload(&self) -> Vec<u8> {
        match self {
            Message::Hello(h) => h.encode(),
            Message::HelloAck => Vec::new(),
            Message::Ping(nonce) | Message::Pong(nonce) => nonce.to_be_bytes().to_vec(),
            Message::NewTransaction(tx) => tx.serialize_to_bytes(),
            Message::GetAddr => Vec::new(),
            Message::Addr(endpoints) => encode_addrs(endpoints),
        }
    }

    /// Decode a validated frame into a message.
    pub fn decode(raw: &RawFrame) -> Result<Message, MessageError> {
        let len = raw.payload.len();
        match raw.msg_type {
            type_code::HELLO => Hello::decode(&raw.payload)
                .map(Message::Hello)
                .ok_or(MessageError::BadLength {
                    msg_type: raw.msg_type,
                    len,
                }),
            type_code::HELLO_ACK => {
                if len == 0 {
                    Ok(Message::HelloAck)
                } else {
                    Err(MessageError::BadLength {
                        msg_type: raw.msg_type,
                        len,
                    })
                }
            }
            type_code::PING | type_code::PONG => {
                let bytes: [u8; 8] = raw
                    .payload
                    .as_slice()
                    .try_into()
                    .map_err(|_| MessageError::BadLength {
                        msg_type: raw.msg_type,
                        len,
                    })?;
                let nonce = u64::from_be_bytes(bytes);
                if raw.msg_type == type_code::PING {
                    Ok(Message::Ping(nonce))
                } else {
                    Ok(Message::Pong(nonce))
                }
            }
            type_code::NEW_TRANSACTION => Transaction::from_bytes(&raw.payload)
                .map(Message::NewTransaction)
                .ok_or(MessageError::BadTransaction),
            type_code::GET_ADDR => {
                if len == 0 {
                    Ok(Message::GetAddr)
                } else {
                    Err(MessageError::BadLength {
                        msg_type: raw.msg_type,
                        len,
                    })
                }
            }
            type_code::ADDR => decode_addrs(&raw.payload)
                .map(Message::Addr)
                .ok_or(MessageError::BadAddrs),
            other => Err(MessageError::UnknownType(other)),
        }
    }
}

/// Write one message as a `JEFY` frame at the current protocol version.
pub async fn write_message<W: AsyncWrite + Unpin>(
    writer: &mut W,
    message: &Message,
) -> Result<(), FrameError> {
    let payload = message.encode_payload();
    write_frame(
        writer,
        super::frame::PROTOCOL_VERSION,
        message.type_code(),
        &payload,
    )
    .await
}

/// Read and decode one message.
pub async fn read_message<R: AsyncRead + Unpin>(
    reader: &mut R,
) -> Result<Message, MessageError> {
    let raw = read_frame(reader).await?;
    Message::decode(&raw)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_tx() -> Transaction {
        Transaction {
            sender: [1u8; 32],
            recipient: [2u8; 32],
            input: 1000,
            output: 950,
            nonce: 7,
            signature: [9u8; 64],
        }
    }

    async fn round_trip(message: Message) -> Message {
        let mut buf: Vec<u8> = Vec::new();
        write_message(&mut buf, &message).await.unwrap();
        read_message(&mut buf.as_slice()).await.unwrap()
    }

    #[tokio::test]
    async fn hello_round_trips() {
        let hello = Hello {
            version: 1,
            nonce: 0xDEAD_BEEF_CAFE_1234,
            timestamp: 1_700_000_000,
            listen_port: 8333,
            services: 0,
        };
        match round_trip(Message::Hello(hello.clone())).await {
            Message::Hello(got) => assert_eq!(got, hello),
            other => panic!("expected Hello, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn hello_ack_round_trips() {
        assert!(matches!(
            round_trip(Message::HelloAck).await,
            Message::HelloAck
        ));
    }

    #[tokio::test]
    async fn ping_and_pong_round_trip() {
        assert!(matches!(round_trip(Message::Ping(42)).await, Message::Ping(42)));
        assert!(matches!(round_trip(Message::Pong(99)).await, Message::Pong(99)));
    }

    #[tokio::test]
    async fn new_transaction_round_trips() {
        let tx = sample_tx();
        match round_trip(Message::NewTransaction(tx.clone())).await {
            Message::NewTransaction(got) => {
                assert_eq!(got.serialize_to_bytes(), tx.serialize_to_bytes());
            }
            other => panic!("expected NewTransaction, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn unknown_type_is_reported() {
        let raw = RawFrame {
            version: 1,
            msg_type: 0x7FFF,
            payload: Vec::new(),
        };
        assert!(matches!(
            Message::decode(&raw),
            Err(MessageError::UnknownType(0x7FFF))
        ));
    }
}
