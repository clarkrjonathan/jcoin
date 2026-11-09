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
  - `src/lib.rs` — core `Transaction` (type-tagged, fixed-width, Ed25519
    signing + verification, SHA-256 tx id, coinbase constructor).
  - `src/state.rs` — account state table (balance + nonce) and `state_root`.
  - `src/block.rs` — header, block, PoW, genesis, mining, validation.
  - `src/chain.rs` — block store and canonical-chain selection by cumulative work.
  - `src/node.rs` — node core: the `NodeRequest` set and the `run_node` task
    owning the chain, mempool, and seen set.
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

## What is built

Starting from the initial vertical (wallet → REST → node core over an
`mpsc`/`oneshot` channel), the following now works:

- **P2P network layer** (`src/p2p/`, see `docs/p2p-protocol.md`): transaction
  gossip over direct TCP (`JEFY` frame codec, symmetric `HELLO`, `PING`/`PONG`,
  `NEW_TRANSACTION`); peer discovery (`GET_ADDR`/`ADDR`, address book, auto-dial;
  `--advertise`, `--max-peers`); and the relay transport (`relay` binary, a
  multiplexing router pairing channels by ephemeral handle; `--relay`,
  `--relay-open`). The relay also serves a handle directory (`LIST`/`HANDLES`)
  so an all-CGNAT network self-bootstraps without out-of-band handles, and has
  basic abuse caps (one handle per connection, a per-connection channel limit).
  A relay channel is a `PeerLink` like any other. Deploying the relay cheaply:
  see `docs/DEPLOY-RELAY.md` and `deploy/`; build it lean with
  `cargo build --release --no-default-features --bin relay` (drops the wallet's
  TLS stack).
- **Transactions** (`src/lib.rs`): type-tagged `type/sender/recipient/amount/fee/
  nonce(u64)/signature`, 153 bytes; normal (signed) and coinbase (unsigned).
- **State** (`src/state.rs`): account balances + nonce, canonical `state_root`.
- **Blocks** (`src/block.rs`): 156-byte header, coinbase + ≤1 transfer, PoW
  `SHA256(header) < target`, genesis, mining, validation.
- **Amounts + reward** (`src/amount.rs`, `block_reward` in `src/block.rs`): all
  amounts are `u64` base units; 1 JEF = `COIN` = 100,000,000. The wallet takes
  decimal JEF on the command line. The coinbase may claim
  `block_reward(height) + fees`, where `block_reward = INITIAL_REWARD (50 JEF)
  >> (height / HALVING_INTERVAL)` and `HALVING_INTERVAL` = 32,000 blocks (about
  111 days at 5-minute blocks). The subsidy reaches 0 after 33 halvings (about
  10 years); total supply is just under 3,200,000 JEF, about 89% of it minted
  in year 1.
- **Difficulty + timestamps**: epoch-based retarget toward `TARGET_BLOCK_TIME`
  (5 min). Difficulty is constant within a `RETARGET_WINDOW`-block epoch and
  recomputed only at a boundary from the completed epoch's span, clamped to
  4x/step, never easier than `POW_LIMIT` (the easy start/cap). Adjusting only
  after a full epoch converges without overshoot. Median-time-past is a consensus
  lower bound on timestamps (in `chain.accept`); a 2-hour future bound is a local
  admission rule (in the node core, not in deterministic validation). Genesis
  carries a realistic timestamp so the first epoch measures a real span.
- **Chain + fork choice** (`src/chain.rs`): hash-keyed block store, canonical tip
  by **greatest cumulative work** (U256), reorg as a pointer move.
- **Chain sync**: `NEW_BLOCK` gossip, and `GET_BLOCKS` (block locator) → `BLOCKS`
  for initial download and orphan catch-up.
- **Node** (`src/bin/node.rs`): full nonce/balance validation on submit (ADR
  000), `--mine`/`--miner`, `/chain` and real `/balance`; the wallet fetches the
  nonce and refuses to overspend.

Verified by unit tests and integration tests (direct + relay gossip, late-joiner
sync, work-based reorg), plus live multi-node runs.

### Not yet built / open

Reorg mempool re-add (txs dropped by a reorg are not re-queued), relay handle
rotation and per-channel flow control, block/consensus-version signaling,
and misbehaviour scoring/banning. Persistence stores blocks only (state is
replayed) and the log is never compacted. `POW_LIMIT` is machine/build-specific
(calibrated from a debug-build hash rate).

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
  transaction new alice <recipient-hex-64> 12.5 --fee 0.01
cargo run --bin wallet -- --node http://127.0.0.1:3001 \
  transaction status <tx_id>   # a tx sent to A is visible from B
```

Wallet keys are stored as a hex seed under `--key-dir` (default `.jcoin-keys`),
which is git-ignored.
