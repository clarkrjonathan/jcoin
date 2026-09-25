use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Deserialize, Serialize)]
pub struct Transaction {
    #[serde(with = "hex::serde")]
    pub sender: [u8; 32],

    #[serde(with = "hex::serde")]
    pub recipient: [u8; 32],

    pub input: u64,
    pub output: u64,
    pub nonce: i32,

    #[serde(with = "hex::serde")]
    pub signature: [u8; 64],
}
