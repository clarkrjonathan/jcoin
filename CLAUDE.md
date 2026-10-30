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
  - `src/p2p/` — peer-to-peer layer: `frame` (JEFY codec), `message` (gossip-v1
    messages), `addr` (Endpoint/handle encoding), `link` (the `PeerLink` seam),
    `peer` (handshake, run loop, `run_peer`), `network` (peer registry, address
    book, gossip fan-out, dialer), `relay` (outer transport framing),
    `relay_server` (the multiplexing router), `relay_client` (node-side relay
    connection).
  - `src/bin/node.rs` — axum REST server plus the P2P listener, dialers, and
    optional relay client.
  - `src/bin/wallet.rs` — clap wallet CLI.
  - `src/bin/relay.rs` — the relay server (thin wrapper over `relay_server`).
- `spike/` — **frozen scratchwork**. The original `rust-crud-api` spike where the
  wallet → REST → node → signature-verify path was first proven. Do not build on
  it; it is kept for reference only.
- `docs/p2p-protocol.md` — the peer-to-peer wire protocol. Gossip v1 (frame
  envelope, symmetric `HELLO` handshake, `PING`/`PONG`, `NEW_TRANSACTION`,
  `GET_ADDR`/`ADDR`) and the outer relay transport (multiplexing router with
  ephemeral directory handles, `Direct`/`Relay` addressing) are settled at the
  architecture level; byte encodings and tuning values remain.

## Current scope: the Initial Vertical

A clearly-defined proof-of-concept slice (from `Design/Initial vertical.md`):

- Wallet CLI builds and signs a transaction and POSTs it to a node.
- Node parses it, hands it to the node core over an `mpsc` channel, and awaits a
  `oneshot` reply. The core owns all state (no locks).
- Validation is **signature only**. Nonce and balance checks are intentionally
  not performed yet. Balance and status endpoints return dummy values.

### Built since the vertical

A working P2P network layer (`src/p2p/`, see `docs/p2p-protocol.md`):

- Transaction gossip over **direct TCP**: `JEFY` frame codec, symmetric `HELLO`
  handshake, `PING`/`PONG` heartbeat, `NEW_TRANSACTION` relayed to peers except
  the source.
- **Peer discovery**: `GET_ADDR` / `ADDR` with an address book and auto-dial of
  learned direct peers (`--advertise`, `--max-peers`).
- **Relay transport**: a multiplexing router (`relay` binary) pairs node
  channels by ephemeral directory handle and forwards opaque `DATA`; nodes
  connect with `--relay` and bootstrap with `--relay-open <handle>`. A relayed
  channel is a `PeerLink` like any other, so gossip and discovery work through
  it unchanged.

Verified by unit tests plus two-node direct and relay gossip integration tests.

### Explicitly out of scope (later phases)

Proof of work / mining, balance and nonce validation, persistence / database,
blocks and the chain, difficulty, and the `type/amount/fee` transaction freeze.
Open tuning within the P2P layer: relay handle rotation cadence and per-channel
flow control (head-of-line blocking is noted in `docs/p2p-protocol.md`).

## Build and run

```
cd jcoin
cargo build

# Terminal A: node A (REST on :3000, P2P on :8333)
cargo run --bin node -- --rest-addr 127.0.0.1:3000 --p2p-addr 127.0.0.1:8333

# Terminal B: node B, dialing node A
cargo run --bin node -- --rest-addr 127.0.0.1:3001 --p2p-addr 127.0.0.1:8334 \
  --peer 127.0.0.1:8333

# Terminal C: use the wallet against either node
cargo run --bin wallet -- key new alice
cargo run --bin wallet -- --node http://127.0.0.1:3000 \
  transaction new alice <recipient-hex-64> 1000 --fee 50
cargo run --bin wallet -- --node http://127.0.0.1:3001 \
  transaction status <tx_id>   # a tx sent to A is visible from B
```

Wallet keys are stored as a hex seed under `--key-dir` (default `.jcoin-keys`),
which is git-ignored.
