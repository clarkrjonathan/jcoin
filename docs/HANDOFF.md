# JeffyCoin — session handoff

A snapshot for the next session (or person). Pairs with `../CLAUDE.md` (repo
guide) and `p2p-protocol.md` (wire protocol). Design notes live in the Obsidian
vault at `Chudjacknostim-Reorganized/Projects/JeffyCoin/`.

## Status at a glance

- Branch: `initial-vertical`. **Nothing is pushed to GitHub** — all local.
- Build: `cd jcoin && cargo build` is clean. `cargo test --lib` = **49 passing**.
- The working tree shows modifications under `spike/` — those predate this work
  and are not part of it. `spike/` is frozen scratchwork; ignore it.

## What JeffyCoin is now

A working account-model proof-of-work coin in Rust. A full transaction cycle
runs end to end across gossiping nodes: a wallet signs a transfer, submits it by
REST, it gossips into a mempool, a miner includes it in a block, the block
propagates, and balances settle consistently on a node that never saw the wallet.

### Implemented

- **Transactions** (`src/lib.rs`): type-tagged `type | sender | recipient |
  amount | fee | nonce(u64) | signature`, 153 bytes; normal (Ed25519-signed) and
  coinbase (unsigned). Fixed-width canonical serialization.
- **State** (`src/state.rs`): per-key balance + nonce in a `BTreeMap`; canonical
  `state_root` = SHA-256 of the ordered serialization.
- **Blocks** (`src/block.rs`): 156-byte header; body is a coinbase then zero or
  one transfer; PoW `SHA256(header) < target`; genesis with the marker
  "I am 20 years old, I love Jeffy!"; mining (`assemble_block` + `mine_chunk`,
  interruptible); full `validate_and_apply`.
- **Difficulty + timestamps**: epoch-based retarget toward `TARGET_BLOCK_TIME`
  (300 s). Difficulty is constant within a `RETARGET_WINDOW` (12) epoch,
  recomputed only at a boundary from that epoch's span, clamped to 4x/step, never
  easier than `POW_LIMIT`. Median-time-past (last 11) is a consensus lower bound
  on timestamps (in `chain.accept`); a 2-hour future bound is a local admission
  rule in the node core (never in deterministic validation, so replay is stable).
- **Chain + fork choice** (`src/chain.rs`): hash-keyed block store; each block
  keeps height, cumulative work (`U256`), and resulting state; canonical tip is
  greatest cumulative work; reorg is a pointer move.
- **Chain sync**: `NEW_BLOCK` gossip; a behind node sends a block locator via
  `GET_BLOCKS` and receives `BLOCKS` (initial download and orphan catch-up).
- **P2P** (`src/p2p/`): `JEFY` frame codec (`frame`), gossip messages
  (`message`), the `PeerLink` seam (`link`), handshake + heartbeat + run loop
  (`peer`), peer registry / address book / dialer (`network`), peer discovery
  (`GET_ADDR` / `ADDR`), and the relay transport — a multiplexing router
  (`relay_server`, the `relay` binary) that pairs node channels by ephemeral
  handle, serves a handle directory (`LIST`/`HANDLES`) so an all-CGNAT network
  self-bootstraps, and forwards opaque `DATA`, with the node-side client in
  `relay_client`. Deployment: `docs/DEPLOY-RELAY.md` + `deploy/` (systemd unit,
  Dockerfile); build lean with `--no-default-features --bin relay`.
- **Persistence** (`src/store.rs`): append-only block log; state is replayed on
  startup, so blocks are the single source of truth. Enable with `--data-dir`.
- **Node** (`src/bin/node.rs`): node-core task + axum REST + P2P listener/dialers
  + optional relay client + optional miner. Full nonce/balance validation on
  submit (ADR 000). `/chain` exposes height, tip, and the difficulty target.
- **Wallet** (`src/bin/wallet.rs`): clap CLI; `key new/show`, `transaction
  new/status`; fetches nonce+balance and refuses to overspend.

### Tests

`cargo test --lib` (49). Highlights: frame/message/addr/relay round-trips;
handshake + self-connection; gossip (direct and through the relay); late-joiner
sync; work-based reorg; retarget hardening/cap; median-time-past rejection; an
adversarial block battery (bad PoW, wrong target, bad tx hash, bad body, bad
signature, overspend). Tests use `cfg(test)` easy targets and a zero genesis
timestamp so they mine in milliseconds.

There is also `src/bin/attack.rs`, a dev tool that connects over P2P and sends
one crafted-bad message (used to confirm live rejection), and `src/bin/hashbench.rs`,
which measures the hash rate and prints a target for a chosen block time.

## Build and run (multi-node)

```
cd jcoin && cargo build

# Node A (miner). --miner is a 32-byte hex address (a wallet public key).
cargo run --bin node -- --rest-addr 127.0.0.1:3000 --p2p-addr 127.0.0.1:8333 \
  --data-dir ./dataA --mine --miner <pubkey>

# Node B, dialing A
cargo run --bin node -- --rest-addr 127.0.0.1:3001 --p2p-addr 127.0.0.1:8334 \
  --peer 127.0.0.1:8333 --data-dir ./dataB

# Relay (for nodes behind CGNAT): cargo run --bin relay -- --addr 127.0.0.1:9000
#   then start nodes with --relay 127.0.0.1:9000 (and --relay-open <handle> to
#   bootstrap to a known peer handle).

# Wallet
cargo run --bin wallet -- key new alice
cargo run --bin wallet -- --node http://127.0.0.1:3000 \
  transaction new alice <recipient-hex-64> 100
curl -s localhost:3000/chain   # height, tip, difficulty target
```

## Live demo harness (ephemeral)

A demo lives in the session scratchpad (NOT the repo, so it will not survive):
three mining nodes (one each for Noah / Jon / Holden), a random transaction
generator (`txgen.sh`) writing `txgen.log`, a `stop.sh`, and `keys.txt`
(name → address). If you want to keep it, move `txgen.sh` / `stop.sh` into the
repo. As of this handoff the demo is running on the retargeting binary and its
difficulty is ramping correctly toward ~5-minute blocks (it steps ~4x per
12-block epoch). Three miners peg three cores continuously; `stop.sh` ends it.

## Known limitations / not built

See `KNOWN-ISSUES.md` for the full list with severities, including the
security-relevant vulnerabilities (unbounded fork storage, no peer banning,
time-warp, no finality). Summary:

- **Reorg mempool re-add**: transactions dropped by a reorg are not re-queued.
- **Relay**: one TCP connection per node multiplexes all channels, so a stalled
  channel head-of-line-blocks the rest; handle rotation is not implemented.
- **No misbehaviour scoring/banning** (the notes mention a blacklist).
- **No halving**, no block/consensus-version signaling.
- **Persistence** stores blocks only and never compacts the log.
- `POW_LIMIT` is machine/build-specific (calibrated from a debug-build hash rate).

## Gotchas discovered this session (save yourself the time)

- **Difficulty must retarget per epoch, not per block.** A per-block retarget off
  a trailing window overshoots badly (a live run drifted to ~40-minute blocks)
  because it re-hardens every block while the window still holds fast blocks.
  Epoch-based (adjust only after a full epoch at the current difficulty) fixed it.
- **Genesis timestamp vs "now".** The first retarget epoch includes genesis; if
  the genesis timestamp is far from real time, that epoch's span clamps and
  hardening starts one epoch late. Harmless but surprising.
- **Node stdout is block-buffered to a file.** `println!` logs do not appear
  promptly when redirected, so do not debug a node by tailing its log; use a
  deterministic test instead.
- **Port reuse on restart.** A killed node leaves the port in TIME_WAIT briefly;
  a quick restart can fail to bind. Wait a couple of seconds between stop/start.
- `cfg(test)` overrides `POW_LIMIT` (easy) and `GENESIS_TIMESTAMP` (0) so the
  suite stays fast; the real values are in the `cfg(not(test))` arms.

## Suggested next steps

1. Reorg mempool re-add (and surface mempool contents via REST).
2. Relay hardening: handle rotation, per-channel flow control, the rendezvous
   directory described in `p2p-protocol.md`.
3. Misbehaviour scoring / peer banning.
4. Push to GitHub and open the work for the friend nodes.
