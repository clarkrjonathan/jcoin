//! JeffyCoin wallet CLI for the initial vertical.
//!
//! A thin client that holds Ed25519 keys, builds and signs a transaction, and
//! sends it to a node's REST endpoint. It fetches the sender's balance and next
//! nonce from the node first and refuses to overspend.
//!
//! Amounts on the command line are decimal JEF ("12.5"); the wallet converts
//! them to base units (1 JEF = 100,000,000) before signing.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use clap::{Parser, Subcommand};
use ed25519_dalek::{Signer, SigningKey};
use rand::rngs::OsRng;

use jcoin::amount::{format_jef, parse_jef};
use jcoin::Transaction;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

#[derive(Parser)]
#[command(name = "jeffy-wallet", about = "JeffyCoin wallet (initial vertical)")]
struct Cli {
    /// Base URL of the node to talk to.
    #[arg(long, default_value = "http://localhost:3000", global = true)]
    node: String,

    /// Directory holding wallet key files.
    #[arg(long, default_value = ".jcoin-keys", global = true)]
    key_dir: PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Manage wallet keys.
    Key {
        #[command(subcommand)]
        action: KeyAction,
    },
    /// Build, sign, and track transactions.
    Transaction {
        #[command(subcommand)]
        action: TxAction,
    },
}

#[derive(Subcommand)]
enum KeyAction {
    /// Generate a new key (errors if one by that name already exists).
    New { name: String },
    /// Show the public key for a stored key.
    Show { name: String },
}

#[derive(Subcommand)]
enum TxAction {
    /// Create, sign, and send a transaction.
    New {
        /// Name of the stored key to sign with.
        key: String,
        /// Recipient address as 64 hex characters.
        recipient: String,
        /// Amount the recipient receives, in JEF (for example 12.5).
        #[arg(value_parser = parse_jef)]
        amount: u64,
        /// Voluntary fee to the miner, in JEF. The sender pays `amount + fee`.
        #[arg(long, default_value = "0", value_parser = parse_jef)]
        fee: u64,
    },
    /// Poll the node for a transaction's status.
    Status { tx_id: String },
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    if let Err(err) = run(cli).await {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<()> {
    let Cli {
        node,
        key_dir,
        command,
    } = cli;

    match command {
        Command::Key { action } => match action {
            KeyAction::New { name } => {
                let key = generate_key(&key_dir, &name)?;
                println!("Generated key '{name}'");
                println!("public key: {}", hex::encode(key.verifying_key().to_bytes()));
            }
            KeyAction::Show { name } => {
                let key = load_key(&key_dir, &name)?;
                println!("public key: {}", hex::encode(key.verifying_key().to_bytes()));
            }
        },
        Command::Transaction { action } => match action {
            TxAction::New {
                key,
                recipient,
                amount,
                fee,
            } => {
                send_transaction(&node, &key_dir, &key, &recipient, amount, fee).await?;
            }
            TxAction::Status { tx_id } => {
                check_status(&node, &tx_id).await?;
            }
        },
    }
    Ok(())
}

fn key_path(dir: &Path, name: &str) -> PathBuf {
    dir.join(format!("{name}_ed25519"))
}

/// Generate a new key and write its 32-byte seed as hex. Errors if it exists.
fn generate_key(dir: &Path, name: &str) -> Result<SigningKey> {
    fs::create_dir_all(dir)?;
    let path = key_path(dir, name);
    if path.exists() {
        return Err(format!("key '{name}' already exists at {}", path.display()).into());
    }
    let key = SigningKey::generate(&mut OsRng);
    fs::write(&path, hex::encode(key.to_bytes()))?;
    Ok(key)
}

/// Load a stored key by name.
fn load_key(dir: &Path, name: &str) -> Result<SigningKey> {
    let path = key_path(dir, name);
    let hex_seed = fs::read_to_string(&path)
        .map_err(|_| format!("no key '{name}' at {}", path.display()))?;
    let seed: [u8; 32] = hex::decode(hex_seed.trim())?
        .try_into()
        .map_err(|_| "key file must hold a 32-byte seed")?;
    Ok(SigningKey::from_bytes(&seed))
}

/// Decode 64 hex characters into a 32-byte array (accepts an optional 0x prefix).
fn decode_address(input: &str) -> Result<[u8; 32]> {
    let trimmed = input.strip_prefix("0x").unwrap_or(input);
    let bytes = hex::decode(trimmed)?;
    bytes
        .try_into()
        .map_err(|_| "address must be 32 bytes (64 hex characters)".into())
}

async fn send_transaction(
    node: &str,
    key_dir: &Path,
    key_name: &str,
    recipient: &str,
    amount: u64,
    fee: u64,
) -> Result<()> {
    let key = load_key(key_dir, key_name)?;
    let recipient = decode_address(recipient)?;
    let sender = key.verifying_key().to_bytes();

    let client = reqwest::Client::new();

    // Fetch the sender's balance and pending chain from the node. A failed
    // request or a missing field is an error: defaulting to 0 would sign with
    // the wrong nonce.
    let response = client
        .get(format!("{node}/balance/{}", hex::encode(sender)))
        .send()
        .await?;
    if !response.status().is_success() {
        return Err(format!("node returned {} for the balance query", response.status()).into());
    }
    let account = response.json::<serde_json::Value>().await?;
    let field = |name: &str| {
        account[name]
            .as_u64()
            .ok_or_else(|| format!("node reply has no '{name}' field"))
    };
    let balance = field("balance")?;
    // The next free nonce after any pending transactions, so a sender can
    // queue several before a block confirms them.
    let nonce = field("next_nonce")?;
    let pending = field("pending")?;

    let needed = amount.checked_add(fee).ok_or("amount + fee is too large")?;
    let available = balance.saturating_sub(pending);
    if available < needed {
        return Err(format!(
            "balance too low: need {} JEF, have {} JEF ({} JEF confirmed, {} JEF committed to pending transactions)",
            format_jef(needed),
            format_jef(available),
            format_jef(balance),
            format_jef(pending)
        )
        .into());
    }

    let mut tx = Transaction {
        tx_type: jcoin::tx_type::NORMAL,
        sender,
        recipient,
        amount,
        fee,
        nonce,
        signature: [0u8; 64],
    };

    // Sign over everything but the signature, then fill it in.
    let signature = key.sign(&tx.signing_bytes());
    tx.signature = signature.to_bytes();

    let tx_id = tx.tx_id_hex();
    let url = format!("{node}/wallet/transaction");

    let response = client.post(&url).json(&tx).send().await?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();

    println!("tx id: {tx_id}");
    println!("node:  {} ({})", url, status);
    println!("reply: {body}");

    record_sent(key_dir, &tx_id, node)?;
    Ok(())
}

async fn check_status(node: &str, tx_id: &str) -> Result<()> {
    let url = format!("{node}/wallet/transaction/{tx_id}");
    let client = reqwest::Client::new();
    let response = client.get(&url).send().await?;
    let status = response.status();
    let body = response.text().await.unwrap_or_default();
    println!("status ({status}): {body}");
    Ok(())
}

/// Append a one-line record of a sent transaction for later `status` polling.
fn record_sent(dir: &Path, tx_id: &str, node: &str) -> Result<()> {
    fs::create_dir_all(dir)?;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let line = format!("{now}\t{tx_id}\t{node}\n");
    let path = dir.join("sent_transactions.log");
    use std::io::Write;
    let mut file = fs::OpenOptions::new().create(true).append(true).open(path)?;
    file.write_all(line.as_bytes())?;
    Ok(())
}
