//! JeffyCoin core types.
//!
//! The transaction is a single type-tagged format: a normal transfer or an
//! unsigned coinbase. Fields are fixed-width and serialize to raw hex (no `0x`
//! prefix) so the wire format matches the canonical byte layout used for hashing
//! and signing. See `docs/p2p-protocol.md` and the vault design notes.

use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub mod block;
pub mod chain;
pub mod node;
pub mod p2p;
pub mod state;

/// Transaction type codes.
pub mod tx_type {
    /// A normal, signed transfer.
    pub const NORMAL: u8 = 0;
    /// An unsigned coinbase that mints the block reward.
    pub const COINBASE: u8 = 1;
}

/// A JeffyCoin transaction (normal transfer or coinbase).
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Transaction {
    /// Transaction type: `0` normal, `1` coinbase (see `tx_type`).
    #[serde(rename = "type")]
    pub tx_type: u8,

    /// Sender public key (Ed25519), 32 bytes. Zeroed for a coinbase.
    #[serde(with = "hex::serde")]
    pub sender: [u8; 32],

    /// Recipient address / public key, 32 bytes.
    #[serde(with = "hex::serde")]
    pub recipient: [u8; 32],

    /// Amount credited to the recipient (base units).
    pub amount: u64,

    /// Fee to the miner (base units). Zero for a coinbase.
    pub fee: u64,

    /// Per-sender sequence number for replay protection.
    pub nonce: u64,

    /// Ed25519 signature over `signing_bytes()`, 64 bytes. Zeroed for a coinbase.
    #[serde(with = "hex::serde")]
    pub signature: [u8; 64],
}

impl Transaction {
    /// Total serialized size: 1 + 32 + 32 + 8 + 8 + 8 + 64 = 153 bytes.
    pub const SERIALIZED_LEN: usize = 153;

    /// Length of the signed portion: everything except the signature.
    pub const SIGNING_LEN: usize = 89;

    /// Build an unsigned coinbase crediting `recipient` with `amount`.
    pub fn coinbase(recipient: [u8; 32], amount: u64) -> Transaction {
        Transaction {
            tx_type: tx_type::COINBASE,
            sender: [0u8; 32],
            recipient,
            amount,
            fee: 0,
            nonce: 0,
            signature: [0u8; 64],
        }
    }

    /// True if this is a coinbase.
    pub fn is_coinbase(&self) -> bool {
        self.tx_type == tx_type::COINBASE
    }

    /// Full canonical byte serialization, including the signature.
    pub fn serialize_to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(Self::SERIALIZED_LEN);
        bytes.extend_from_slice(&self.signing_bytes());
        bytes.extend_from_slice(&self.signature);
        bytes
    }

    /// Parse a transaction from its canonical serialization. Returns `None` if
    /// the slice is not exactly `SERIALIZED_LEN` bytes.
    pub fn from_bytes(bytes: &[u8]) -> Option<Transaction> {
        if bytes.len() != Self::SERIALIZED_LEN {
            return None;
        }
        Some(Transaction {
            tx_type: bytes[0],
            sender: bytes[1..33].try_into().ok()?,
            recipient: bytes[33..65].try_into().ok()?,
            amount: u64::from_be_bytes(bytes[65..73].try_into().ok()?),
            fee: u64::from_be_bytes(bytes[73..81].try_into().ok()?),
            nonce: u64::from_be_bytes(bytes[81..89].try_into().ok()?),
            signature: bytes[89..153].try_into().ok()?,
        })
    }

    /// Everything except the signature. This is what you hash and sign, since a
    /// signature cannot cover itself.
    pub fn signing_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(Self::SIGNING_LEN);
        bytes.push(self.tx_type);
        bytes.extend_from_slice(&self.sender);
        bytes.extend_from_slice(&self.recipient);
        bytes.extend_from_slice(&self.amount.to_be_bytes());
        bytes.extend_from_slice(&self.fee.to_be_bytes());
        bytes.extend_from_slice(&self.nonce.to_be_bytes());
        bytes
    }

    /// Transaction id: SHA-256 over the full canonical serialization.
    pub fn tx_id(&self) -> [u8; 32] {
        let mut hasher = Sha256::new();
        hasher.update(self.serialize_to_bytes());
        hasher.finalize().into()
    }

    /// Transaction id as a lowercase hex string.
    pub fn tx_id_hex(&self) -> String {
        hex::encode(self.tx_id())
    }

    /// Returns true only if `signature` is a valid Ed25519 signature by `sender`
    /// over `signing_bytes()`. A coinbase is unsigned, so this is not used for it.
    pub fn verify_signature(&self) -> bool {
        let Ok(public_key) = VerifyingKey::from_bytes(&self.sender) else {
            return false; // sender is not a valid curve point
        };
        let signature = Signature::from_bytes(&self.signature);
        public_key
            .verify_strict(&self.signing_bytes(), &signature)
            .is_ok()
    }
}
