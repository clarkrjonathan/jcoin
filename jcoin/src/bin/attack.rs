//! Adversarial test tool: connect to a node over P2P, complete the handshake,
//! and send one crafted message to confirm the node's verification rejects it.
//!
//! Usage: attack <node-p2p-addr> <mode>
//! Modes: badtx-sig | block-badpow | block-badstate | block-badbody |
//!        block-badtxhash | block-good (a valid control that should be accepted)

use std::env;
use std::time::Duration;

use ed25519_dalek::{Signer, SigningKey};
use rand::rngs::OsRng;
use tokio::net::TcpStream;

use jcoin::block::{
    build_and_mine, transactions_hash, Block, BlockHeader, BLOCK_REWARD, BLOCK_VERSION,
    POW_LIMIT,
};
use jcoin::p2p::{handshake, local_hello, write_message, Message};
use jcoin::state::State;
use jcoin::Transaction;

/// Build a validly-signed normal transfer (used for semantic-rejection tests).
fn signed_tx(nonce: u64, amount: u64) -> Transaction {
    let key = SigningKey::generate(&mut OsRng);
    let mut tx = Transaction {
        tx_type: jcoin::tx_type::NORMAL,
        sender: key.verifying_key().to_bytes(),
        recipient: [2u8; 32],
        amount,
        fee: 0,
        nonce,
        signature: [0u8; 64],
    };
    tx.signature = key.sign(&tx.signing_bytes()).to_bytes();
    tx
}

fn remine(header: &mut BlockHeader) {
    while !header.meets_target() {
        header.nonce = header.nonce.wrapping_add(1);
    }
}

#[tokio::main]
async fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() != 3 {
        eprintln!("usage: attack <node-p2p-addr> <mode>");
        std::process::exit(2);
    }
    let addr = &args[1];
    let mode = &args[2];

    let mut stream = TcpStream::connect(addr).await.expect("connect");
    // A fixed non-zero nonce; it only needs to differ from the node's.
    handshake(&mut stream, &local_hello(0xA11CE, 0))
        .await
        .expect("handshake");

    let genesis = Block::genesis();
    let message = match mode.as_str() {
        // A transaction with a zeroed (invalid) signature.
        "badtx-sig" => Message::NewTransaction(Transaction {
            tx_type: jcoin::tx_type::NORMAL,
            sender: [1u8; 32],
            recipient: [2u8; 32],
            amount: 10,
            fee: 0,
            nonce: 0,
            signature: [0u8; 64],
        }),

        // Validly signed but with a bad nonce (account nonce is 0, not 5).
        "tx-badnonce" => {
            let tx = signed_tx(5, 10);
            eprintln!("txid {}", tx.tx_id_hex());
            Message::NewTransaction(tx)
        }

        // Validly signed but the sender has no funds.
        "tx-nofunds" => {
            let tx = signed_tx(0, 10);
            eprintln!("txid {}", tx.tx_id_hex());
            Message::NewTransaction(tx)
        }

        // A structurally valid block that was never mined (hash does not meet target).
        "block-badpow" => {
            let coinbase = Transaction::coinbase([9u8; 32], BLOCK_REWARD);
            let transactions = vec![coinbase];
            let mut state = State::new();
            state.apply(&transactions[0]).unwrap();
            let header = BlockHeader {
                version: BLOCK_VERSION,
                height: 1,
                previous_block_hash: genesis.header.block_hash(),
                transactions_hash: transactions_hash(&transactions),
                state_root: state.state_root(),
                timestamp: 1,
                target: POW_LIMIT,
                nonce: 0, // not mined
            };
            Message::NewBlock(Block { header, transactions })
        }

        // A mined block with a tampered state_root (re-mined so PoW still holds).
        "block-badstate" => {
            let (mut block, _) =
                build_and_mine(&genesis.header, &State::new(), BLOCK_REWARD, [9u8; 32], None, POW_LIMIT, 1)
                    .unwrap();
            block.header.state_root = [0u8; 32];
            remine(&mut block.header);
            Message::NewBlock(block)
        }

        // A mined block with a tampered transactions_hash.
        "block-badtxhash" => {
            let (mut block, _) =
                build_and_mine(&genesis.header, &State::new(), BLOCK_REWARD, [9u8; 32], None, POW_LIMIT, 1)
                    .unwrap();
            block.header.transactions_hash = [0u8; 32];
            remine(&mut block.header);
            Message::NewBlock(block)
        }

        // A block whose body is not coinbase-first (two normal, no coinbase).
        "block-badbody" => {
            let fake = Transaction {
                tx_type: jcoin::tx_type::NORMAL,
                sender: [1u8; 32],
                recipient: [2u8; 32],
                amount: 1,
                fee: 0,
                nonce: 0,
                signature: [0u8; 64],
            };
            let transactions = vec![fake.clone(), fake];
            let mut header = BlockHeader {
                version: BLOCK_VERSION,
                height: 1,
                previous_block_hash: genesis.header.block_hash(),
                transactions_hash: transactions_hash(&transactions),
                state_root: State::new().state_root(),
                timestamp: 1,
                target: POW_LIMIT,
                nonce: 0,
            };
            remine(&mut header);
            Message::NewBlock(Block { header, transactions })
        }

        // A valid block on top of genesis: the control, which must be accepted.
        "block-good" => {
            let (block, _) =
                build_and_mine(&genesis.header, &State::new(), BLOCK_REWARD, [9u8; 32], None, POW_LIMIT, 1)
                    .unwrap();
            Message::NewBlock(block)
        }

        other => {
            eprintln!("unknown mode: {other}");
            std::process::exit(2);
        }
    };

    write_message(&mut stream, &message).await.expect("send");
    // Give the node time to process before we disconnect.
    tokio::time::sleep(Duration::from_millis(400)).await;
    println!("sent {mode}");
}
