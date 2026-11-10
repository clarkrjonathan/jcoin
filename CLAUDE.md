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
  - `src/consensus.rs` — `Params` (every consensus value as data: `MAINNET`,
    `REGTEST`), `block_reward`, and `BlockContext` (what the next block must
    satisfy). There are no `cfg(test)` consensus constants; tests use `REGTEST`.
  - `src/block.rs` — header, block, PoW, genesis, mining, validation.
    `validate_and_apply` takes a `BlockContext` and is the whole deterministic
    rule set.
  - `src/chain.rs` — block store and canonical-chain selection by cumulative
    work. `context_for_child` is the one place the next block's height,
    reward, target, and minimum timestamp are derived.
  - `src/mempool.rs` — the mempool (indexed by id and by sender, then nonce;
    capped). A sender may queue up to 25 pending transactions with
    consecutive nonces, if the confirmed balance covers all of them; a reused
    nonce is a conflict and is rejected. Plus a bounded seen-id cache that
    only answers status queries. `/balance` reports `next_nonce` and
    `pending`, and the wallet signs with `next_nonce`.
  - `src/amount.rs` — base unit (`COIN`) and decimal JEF parse/format.
  - `src/store.rs` — append-only block log; a torn tail is cut on startup.
  - `src/node.rs` — node core: the `NodeRequest` set and the `run_node` task
    owning the chain, mempool, and seen cache.
  - `src/p2p/` — peer-to-peer layer. The JeffyCoin protocol: `frame` (JEFY
    codec), `message`, `addr` (multiaddr list encoding), `link` (the `PeerLink`
    seam), `peer` (handshake, run loop, `run_peer`), `network` (session
    registry, address book, gossip fan-out, dial requests). The transport:
    `host` (the libp2p swarm: TCP + Noise + Yamux, relay client, DCUtR hole
    punching, identify, ping, rendezvous client, and the `/jeffycoin/2.0.0`
    stream that `run_peer` runs on) and `relay_host` (the relay node: circuit
    relay v2 + rendezvous server).
  - `src/bin/node.rs` — axum REST server, the libp2p host, and the miner.
  - `src/bin/wallet.rs` — clap wallet CLI.
  - `src/bin/relay.rs` — the relay node (thin wrapper over `relay_host`).
- `spike/` — **frozen scratchwork**. The original `rust-crud-api` spike where the
  wallet → REST → node → signature-verify path was first proven. Do not build on
  it; it is kept for reference only.
- `docs/p2p-protocol.md` — the peer-to-peer protocol (wire version 2): frame
  envelope, messages, gossip and sync rules, and the libp2p transport (stack,
  identity, relay and discovery flow, and what a node in another language
  needs).
- `demo/` — the persisted demo network: `start.sh`, `stop.sh`, `status.sh`,
  `txgen.sh` (a relay plus three mining nodes: Noah, Jon, and Holden, who is
  relay-only). Its chain data, keys, and logs are git-ignored. **All live
  testing runs on this one persisted chain; do not start fresh chains for
  live tests.**

## What is built

Starting from the initial vertical (wallet → REST → node core over an
`mpsc`/`oneshot` channel), the following now works:

- **P2P network layer** (`src/p2p/`, see `docs/p2p-protocol.md`): libp2p
  carries the unchanged JeffyCoin protocol (`JEFY` frames, symmetric `HELLO`,
  `PING`/`PONG`, gossip, sync, `GET_ADDR`/`ADDR` with multiaddrs) on one
  `/jeffycoin/2.0.0` stream per peer. Every connection is encrypted (Noise) and
  authenticated by peer id (an Ed25519 key in `<data-dir>/node_key`). Nodes
  behind NAT use `--relay <multiaddr>/p2p/<relay id>`: they reserve a circuit
  on the relay, register with its rendezvous service, discover each other, and
  hole punch (DCUtR) to direct connections when possible. `--peer` accepts
  `ip:port` or a multiaddr; `--no-listen` makes a dial-out-only node. Deploying
  the relay: `docs/DEPLOY-RELAY.md` and `deploy/`; build it lean with
  `cargo build --release --no-default-features --bin relay`.
- **Transactions** (`src/lib.rs`): type-tagged `type/sender/recipient/amount/fee/
  nonce(u64)/signature`, 153 bytes; normal (signed) and coinbase (unsigned).
- **State** (`src/state.rs`): account balances + nonce, canonical `state_root`.
- **Blocks** (`src/block.rs`): 156-byte header, coinbase + ≤1 transfer, PoW
  `SHA256(header) < target`, genesis, mining, validation.
- **Amounts + reward** (`src/amount.rs`, `Params::block_reward` in
  `src/consensus.rs`): all amounts are `u64` base units; 1 JEF = `COIN` =
  100,000,000. The wallet takes decimal JEF on the command line. The coinbase
  may claim `block_reward(height) + fees`, where `block_reward = initial_reward
  (50 JEF) >> (height / halving_interval)` and `halving_interval` = 32,000
  blocks on `MAINNET` (about 111 days at 5-minute blocks; `REGTEST` uses 150). The subsidy reaches 0 after 33 halvings (about
  10 years); total supply is just under 3,200,000 JEF, about 89% of it minted
  in year 1.
- **Difficulty + timestamps**: epoch-based retarget toward `target_block_time`
  (5 min). Difficulty is constant within a `retarget_window`-block epoch and
  recomputed only at a boundary from the completed epoch's span, clamped to
  4x/step, never easier than `pow_limit` (the easy start/cap). Adjusting only
  after a full epoch converges without overshoot. Median-time-past is a consensus
  lower bound on timestamps (in `BlockContext::min_timestamp`, checked by
  `validate_and_apply`; the miner uses it as a floor); a 2-hour future bound is a local
  admission rule (in the node core, not in deterministic validation). Genesis
  carries a realistic timestamp so the first epoch measures a real span.
- **Chain + fork choice** (`src/chain.rs`): hash-keyed block store, canonical tip
  by **greatest cumulative work** (U256), reorg as a pointer move.
- **Chain sync**: `NEW_BLOCK` gossip, and `GET_BLOCKS` (block locator) → `BLOCKS`
  for initial download and orphan catch-up.
- **Node** (`src/bin/node.rs`): full nonce/balance validation on submit (ADR
  000), `--mine`/`--miner`, `--regtest`, `/chain` and real `/balance`; the
  wallet fetches the nonce and refuses to overspend. Transaction status is
  `pending`, `seen`, or `unknown`.
- **Validation hardening**: unknown transaction types and block versions are
  rejected; the coinbase must be canonical (zero sender, fee, nonce,
  signature). The address book is capped (1,000, random eviction), one `ADDR`
  is read up to 256 entries, and in-flight dials count toward `--max-peers`.

Verified by unit tests and integration tests (direct libp2p gossip,
relay-only nodes that find each other through rendezvous and gossip over a
circuit, late-joiner sync, work-based reorg), plus live runs on the persisted
demo chain.

### Not yet built / open

Reorg mempool re-add (txs dropped by a reorg are not re-queued automatically;
resubmitting one now works), more than one relay per node, a persisted
address book, block/consensus-version signaling,
and misbehaviour scoring/banning. Persistence stores blocks only (state is
replayed) and the log is never compacted. `MAINNET.pow_limit` is machine/build-specific
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
