//! Wallet keys and transaction signing, shared by the `wallet` command-line
//! tool and the desktop app.
//!
//! A key file holds a 32-byte Ed25519 seed as hex. It is not encrypted: anyone
//! who can read the file can spend from the address.

use std::fs;
use std::path::{Path, PathBuf};

use ed25519_dalek::{Signer, SigningKey};
use rand::rngs::OsRng;

use crate::Transaction;

/// Path of the key file for `name` in `dir`.
pub fn key_path(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}{KEY_SUFFIX}"))
}

/// The file-name suffix of a key file.
const KEY_SUFFIX: &str = "_ed25519";

/// Check a key name. It becomes part of a file name, so it is limited to 1 to
/// 32 letters, digits, `-`, and `_`.
pub fn check_key_name(name: &str) -> Result<(), String> {
    let ok = (1..=32).contains(&name.len())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if ok {
        Ok(())
    } else {
        Err("a key name is 1 to 32 letters, digits, '-' or '_'".to_string())
    }
}

/// Write `key` as the key file for `name`. Errors if the name is taken.
fn store_key(dir: &Path, name: &str, key: &SigningKey) -> Result<(), String> {
    check_key_name(name)?;
    fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    let path = key_path(dir, name);
    if path.exists() {
        return Err(format!("key '{name}' already exists at {}", path.display()));
    }
    fs::write(&path, hex::encode(key.to_bytes()))
        .map_err(|e| format!("cannot write {}: {e}", path.display()))
}

/// Generate a new key and write its 32-byte seed as hex. Errors if it exists.
pub fn generate_key(dir: &Path, name: &str) -> Result<SigningKey, String> {
    let key = SigningKey::generate(&mut OsRng);
    store_key(dir, name, &key)?;
    Ok(key)
}

/// Store an existing key under `name`, from its 32-byte seed as 64 hex
/// characters (the content of a key file). Errors if the name is taken.
pub fn import_key(dir: &Path, name: &str, seed_hex: &str) -> Result<SigningKey, String> {
    let seed: [u8; 32] = hex::decode(seed_hex.trim())
        .map_err(|_| "a secret key is 64 hex characters".to_string())?
        .try_into()
        .map_err(|_| "a secret key is 64 hex characters".to_string())?;
    let key = SigningKey::from_bytes(&seed);
    store_key(dir, name, &key)?;
    Ok(key)
}

/// Names of the keys stored in `dir`, sorted. A missing folder has no keys.
pub fn list_keys(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|entry| {
            let file = entry.file_name().into_string().ok()?;
            let name = file.strip_suffix(KEY_SUFFIX)?;
            check_key_name(name).is_ok().then(|| name.to_string())
        })
        .collect();
    names.sort();
    names
}

/// Load a stored key by name.
pub fn load_key(dir: &Path, name: &str) -> Result<SigningKey, String> {
    let path = key_path(dir, name);
    let hex_seed = fs::read_to_string(&path)
        .map_err(|_| format!("no key '{name}' at {}", path.display()))?;
    let seed: [u8; 32] = hex::decode(hex_seed.trim())
        .map_err(|e| format!("key file {} is not hex: {e}", path.display()))?
        .try_into()
        .map_err(|_| "key file must hold a 32-byte seed".to_string())?;
    Ok(SigningKey::from_bytes(&seed))
}

/// Load the key `name`, creating it first if it does not exist.
pub fn load_or_generate_key(dir: &Path, name: &str) -> Result<SigningKey, String> {
    if key_path(dir, name).exists() {
        load_key(dir, name)
    } else {
        generate_key(dir, name)
    }
}

/// The address (public key) of a key.
pub fn address_of(key: &SigningKey) -> [u8; 32] {
    key.verifying_key().to_bytes()
}

/// Decode 64 hex characters into a 32-byte address (accepts a `0x` prefix).
pub fn decode_address(input: &str) -> Result<[u8; 32], String> {
    let trimmed = input.trim();
    let trimmed = trimmed.strip_prefix("0x").unwrap_or(trimmed);
    hex::decode(trimmed)
        .map_err(|e| format!("address is not hex: {e}"))?
        .try_into()
        .map_err(|_| "address must be 32 bytes (64 hex characters)".to_string())
}

/// Build and sign a normal transfer from `key`.
pub fn sign_transfer(
    key: &SigningKey,
    recipient: [u8; 32],
    amount: u64,
    fee: u64,
    nonce: u64,
) -> Transaction {
    let mut tx = Transaction {
        tx_type: crate::tx_type::NORMAL,
        sender: address_of(key),
        recipient,
        amount,
        fee,
        nonce,
        signature: [0u8; 64],
    };
    // Sign over everything but the signature, then fill it in.
    tx.signature = key.sign(&tx.signing_bytes()).to_bytes();
    tx
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signed_transfer_verifies_and_keys_round_trip() {
        let dir = std::env::temp_dir().join(format!("jcoin-wallet-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        let key = load_or_generate_key(&dir, "a").unwrap();
        let again = load_or_generate_key(&dir, "a").unwrap();
        assert_eq!(address_of(&key), address_of(&again));
        assert!(generate_key(&dir, "a").is_err());

        let tx = sign_transfer(&key, [7u8; 32], 5, 1, 3);
        assert!(tx.verify_signature());
        assert_eq!(tx.sender, address_of(&key));
        assert_eq!(decode_address(&hex::encode([7u8; 32])).unwrap(), [7u8; 32]);
        assert!(decode_address("abcd").is_err());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn keys_are_listed_imported_and_name_checked() {
        let dir = std::env::temp_dir().join(format!("jcoin-wallet-list-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        assert!(list_keys(&dir).is_empty());
        let b = generate_key(&dir, "b").unwrap();
        generate_key(&dir, "a").unwrap();
        fs::write(dir.join("sent_transactions.log"), "x").unwrap();
        assert_eq!(list_keys(&dir), vec!["a", "b"]);

        // Importing a key file's content gives the same address.
        let copy = import_key(&dir, "b-copy", &hex::encode(b.to_bytes())).unwrap();
        assert_eq!(address_of(&copy), address_of(&b));
        assert!(import_key(&dir, "b", &hex::encode(b.to_bytes())).is_err(), "name taken");
        assert!(import_key(&dir, "short", "abcd").is_err());

        for bad in ["", "../x", "a/b", "has space", &"n".repeat(33)] {
            assert!(generate_key(&dir, bad).is_err(), "{bad:?} must be rejected");
        }
        let _ = fs::remove_dir_all(&dir);
    }
}
