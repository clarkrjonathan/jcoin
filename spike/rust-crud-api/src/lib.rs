use serde::{Deserialize, Serialize};
use serde_json::json;
use primitive_types::{U256, U512};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};

#[derive(Deserialize, Serialize)]
pub struct Transaction {
    pub sender: U256,

    pub recipient: U256,

    pub input: u64,
    pub output: u64,
    pub nonce: u32,

    pub signature: U512,
}

impl Transaction {
    /// Total size: 32 + 32 + 8 + 8 + 4 + 64 = 148 bytes
    pub const SERIALIZED_LEN: usize = 148;

    pub fn serialize_to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(Self::SERIALIZED_LEN);

        bytes.extend_from_slice(&self.signing_bytes());

        let mut signature = [0u8; 64];
        self.signature.to_big_endian(&mut signature);
        bytes.extend_from_slice(&signature);

        bytes
    }

    /// Everything except the signature. This is what you hash and sign,
    /// since a signature can't cover itself.
    pub fn signing_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(84);

        let mut sender = [0u8; 32];
        self.sender.to_big_endian(&mut sender);
        bytes.extend_from_slice(&sender);

        let mut recipient = [0u8; 32];
        self.recipient.to_big_endian(&mut recipient);
        bytes.extend_from_slice(&recipient);

        bytes.extend_from_slice(&self.input.to_be_bytes());
        bytes.extend_from_slice(&self.output.to_be_bytes());
        bytes.extend_from_slice(&self.nonce.to_be_bytes());

        bytes
    }

        /// Returns true only if `signature` is a valid Ed25519 signature
    /// by `sender` over `signing_bytes()`.
    pub fn verify_signature(&self) -> bool {
        // sender -> public key
        let mut pk_bytes = [0u8; 32];
        self.sender.to_big_endian(&mut pk_bytes);
        let Ok(public_key) = VerifyingKey::from_bytes(&pk_bytes) else {
            return false; // not a valid curve point
        };

        // signature -> Signature
        let mut sig_bytes = [0u8; 64];
        self.signature.to_big_endian(&mut sig_bytes);
        let signature = Signature::from_bytes(&sig_bytes);

        // Ed25519 hashes the message internally, so pass the raw bytes
        public_key
            .verify_strict(&self.signing_bytes(), &signature)
            .is_ok()
    }
}