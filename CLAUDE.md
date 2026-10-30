# JeffyCoin (jcoin) — guide for Claude

This repo is the **code** for JeffyCoin, Jonathan's hobby cryptocurrency and
blockchain testbed, written in Rust. It is a learning project: Jonathan writes
or reviews the design, and the goal is to understand both Rust and blockchain
mechanics deeply, not to ship a product.

## Where the design lives

The authoritative design, specs, and decisions are in Jonathan's Obsidian vault,
not in this repo:

- Vault path: `../Chudjacknostim-Reorganized/Projects/JeffyCoin/`
  (relative to this repo at `~/Documents/senior_year/jcoin`).
- Key notes: `Design/Initial vertical.md`, `Design/Wallet Client Design.md`,
  `Design/Network Communication.md`, `Specs/` (incl. `ChatGPT Specs.md`),
  `ADR/000-What-Makes-Tx-Invalid-For-Relay.md`.
- The vault has its own `CLAUDE.md` with strict rules (most notes are read-only;
  `Personal/Daily Notes/` is private). Follow those rules when touching the vault.

## Decisions locked

- **State model: account / balance** (per-key balance + nonce). UTXO was
  considered and remains a possible v2, but v1 is the account model.
- Transaction field set for now is the prototype shape:
  `sender, recipient, input, output, nonce, signature` (fee = `input - output`).
  The newer `type/amount/fee` spec model is a later freeze, not yet adopted.

## Repo layout

- `jcoin/` — the real package (current work). Cargo package `jcoin`.
  - `src/lib.rs` — core `Transaction` (fixed-width serialization, Ed25519
    signing bytes + verification, SHA-256 tx id).
  - `src/node.rs` — node core: `NodeRequest`/`SubmitResult`/`StatusResult` and
    the `run_node` task.
  - `src/bin/node.rs` — axum REST server.
  - `src/bin/wallet.rs` — clap wallet CLI.
- `spike/` — **frozen scratchwork**. The original `rust-crud-api` spike where the
  wallet → REST → node → signature-verify path was first proven. Do not build on
  it; it is kept for reference only.

## Current scope: the Initial Vertical

A clearly-defined proof-of-concept slice (from `Design/Initial vertical.md`):

- Wallet CLI builds and signs a transaction and POSTs it to a node.
- Node parses it, hands it to the node core over an `mpsc` channel, and awaits a
  `oneshot` reply. The core owns all state (no locks).
- Validation is **signature only**. Nonce and balance checks are intentionally
  not performed yet. Balance and status endpoints return dummy values.
- A valid transaction is passed to a `relay_handler` stub (the "listener type"
  from the notes) that will later gossip to peers over TCP.

### Explicitly out of scope (later phases)

Proof of work / mining, real P2P / TCP gossip, the AWS CGNAT relay, balance and
nonce validation, persistence / database, blocks and the chain, difficulty, peer
discovery, and the `type/amount/fee` transaction freeze.

## Build and run

```
cd jcoin
cargo build

# Terminal A: start the node (defaults to 0.0.0.0:3000; override with JCOIN_NODE_ADDR)
cargo run --bin node

# Terminal B: use the wallet
cargo run --bin wallet -- key new alice
cargo run --bin wallet -- transaction new alice <recipient-hex-64> 1000 --fee 50
cargo run --bin wallet -- transaction status <tx_id>
```

Wallet keys are stored as a hex seed under `--key-dir` (default `.jcoin-keys`),
which is git-ignored.
