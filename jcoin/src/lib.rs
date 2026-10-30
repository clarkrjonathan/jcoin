//! JeffyCoin core types for the initial vertical.
//!
//! Scope for this slice: a single transaction type with fixed-width canonical
//! serialization, Ed25519 signing bytes, and signature verification. No blocks,
//! no proof of work, no state/balance validation yet. See `CLAUDE.md`.

use ed25519_dalek::{Signature, VerifyingKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub mod node;

/// A JeffyCoin transaction.
///
/// Fields are fixed-width and serialize to raw hex (no `0x` prefix) so the wire
/// format matches the canonical byte layout used for hashing and signing. This
/// is the prototype field set (`sender, recipient, input, output, nonce,
/// signature`) from the "Initial vertical" design note; the newer
/// `type/amount/fee` model is a later spec freeze.
#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Transaction {
    /// Sender public key (Ed25519), 32 bytes.
    #[serde(with = "hex::serde")]
    pub sender: [u8; 32],

    /// Recipient address / public key, 32 bytes.
    #[serde(with = "hex::serde")]
    pub recipient: [u8; 32],

    /// Amount debited from the sender (base units).
    pub input: u64,

    /// Amount credited to the recipient (base units). `input - output` is the fee.
    pub output: u64,

    /// Per-sender sequence number for replay protection. Not validated in this slice.
    pub nonce: u32,

    /// Ed25519 signature over `signing_bytes()`, 64 bytes.
    #[serde(with = "hex::serde")]
    pub signature: [u8; 64],
}

impl Transaction {
    /// Total serialized size: 32 + 32 + 8 + 8 + 4 + 64 = 148 bytes.
    pub const SERIALIZED_LEN: usize = 148;

    /// Length of the signed portion: everything except the signature.
    pub const SIGNING_LEN: usize = 84;

    /// Full canonical byte serialization, including the signature.
    pub fn serialize_to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(Self::SERIALIZED_LEN);
        bytes.extend_from_slice(&self.signing_bytes());
        bytes.extend_from_slice(&self.signature);
        bytes
    }

    /// Everything except the signature. This is what you hash and sign, since a
    /// signature cannot cover itself.
    pub fn signing_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(Self::SIGNING_LEN);
        bytes.extend_from_slice(&self.sender);
        bytes.extend_from_slice(&self.recipient);
        bytes.extend_from_slice(&self.input.to_be_bytes());
        bytes.extend_from_slice(&self.output.to_be_bytes());
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
    /// over `signing_bytes()`.
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
