# JeffyCoin — session handoff

A snapshot for the next session (or person). Pairs with `../CLAUDE.md` (repo
guide) and `p2p-protocol.md` (wire protocol). Design notes live in the Obsidian
vault at `Chudjacknostim-Reorganized/Projects/JeffyCoin/`.

## Status at a glance

- Branch: `initial-vertical`. **Nothing is pushed to GitHub** — all local.
- Build: `cd jcoin && cargo build` is clean. `cargo test --lib` = **85 passing**.
- The working tree shows modifications under `spike/` — those predate this work
  and are not part of it. `spike/` is frozen scratchwork; ignore it.

## Where the last session (2026-11-11) stopped

Read this first. The sections below it are older and partly out of date; the
current sources are `CHANGELOG.md` (what is released), `CHANGELOG-PLANNED.md`
(what is planned, with causes), and `docs/app.md` (the desktop app).

**State of the code**

- Released: `v2.2.0` (tag on `main`): the chain restart. New genesis
  2026-11-11 19:05 UTC, block version 3, corrected difficulty rule (window
  30), stall fallback (target doubles 20 minutes after the parent, then every
  10 minutes), timestamps at most 10 minutes ahead, newer-rules banner.
- Work goes on a descriptive branch, is merged into `main`, and a `v*` tag on
  `main` starts the release build. The tag must equal the version in
  `app/Cargo.toml`. `initial-vertical` is an old branch name, no longer used.
- Decided by Jonathan: **no required updates.** The app never refuses to run
  or mine because of its version; it only prompts harder from network
  evidence.

**After the restart**

- The chain of 2.0.0 to 2.1.2 stalled at height 69 (its difficulty rule
  compounded). It is abandoned. Clients on 2.1.2 or earlier stay on it and
  are not prompted; their owners have to be told to update.
- Next planned: 2.2.1 (logs), 2.2.2 (network and mining view), 2.2.3 (wallet
  and block detail), 2.3 (graphical overhaul). See `CHANGELOG-PLANNED.md`,
  including "Consensus backlog".
- `cargo test --release stall_report -- --ignored --nocapture` in `jcoin/`
  prints the stall simulations the settings were chosen from.

**What exists now**

- Desktop app `app/` (`jcoin-app`): node + wallet + GPU miner, keys, GPU use
  limit, Developer section, updater, per-user install, Sentry reporting.
- `jcoin::service` (embeddable node), `jcoin::wallet`, `jcoin::log`;
  `GET /peers`; pending transfers are re-announced to peers every 60 s.
- Sentry: project `iowa-state-pe/jcoin-app`. Read logs with the
  `sentry-logs` skill (`.claude/skills/sentry-logs/`, linked into
  `~/.claude/skills/jcoin-sentry-logs`) and `tools/sentry-logs`. The reader
  needs a user token in `~/.config/jeffycoin/sentry-token`; none exists yet,
  and its request to Sentry is untested.
- Relay: AWS Lightsail `3.14.123.27:9000`, ssh `ubuntu@3.14.123.27` with
  `~/jcoin-relay.pem`. It runs the pre-2.1.1 relay program.

**Not verified on Windows**

- 2.1.2's install flow and Start menu shortcut, and the in-app updater, have
  not been confirmed on a Windows PC. 2.1.1's relay reservation fix was
  confirmed from a friend's logs.

**Pending operations** (also in `CHANGELOG-PLANNED.md`)

- iMac (`miner@jcoin-miner.local`, LAN only) still runs a build with the
  earlier 35-bit genesis; it cannot sync with 2.x nodes. Redeploy from the
  home network.
- Redeploy the relay; add the `SENTRY_AUTH_TOKEN` repository secret; create
  the Sentry read token.

**On the laptop**

- The installed app runs from `~/.local/bin/jeffycoin`, data in
  `~/.local/share/jeffycoin`.
- The old regtest demo (`demo/`, relay + 3 nodes + txgen) is still running
  and is stalled at height 99 by the same difficulty bug. `demo/stop.sh`
  stops it. `demo/archive-v1-*` holds the v1 chain.
- The NVIDIA GPU is off (`prime-select` is `intel`); mining uses the Intel
  GPU.

**Rules (unchanged)**: never `cargo fmt`; no Claude attribution in commits;
push, merge, and tag only when told to.

## Earlier session (2026-11-09), kept for reference

### Dedicated miner (iMac)

The dedicated miner is an iMac14,1 (late 2013, 4 cores, 8 GB RAM, 256 GB
Apple SSD). It runs Debian 13 from its **internal SSD** (installed
2026-11-09). The image, its build configuration, and the console password are
in `~/Documents/senior_year/jcoin-miner-image/` (see the README there).
- Login: `ssh miner@jcoin-miner.local` (key `~/.ssh/id_ed25519`, passwordless
  sudo, key-only SSH). Wired Ethernet `enp3s0f0`, DHCP (10.0.0.88 at install).
- It boots Debian without a keyboard. The SD card is no longer used: Linux
  cannot drive this iMac's Broadcom SD reader (command timeouts), so the image
  was written over the internal SSD from the old Ubuntu, which is gone.
- Verified on the hardware: boot, DHCP, SSH, sudo, root filesystem grown to
  the full SSD, `screen-off.service` active with backlight 0, no failed units.
  The only boot errors are from the unused Wi-Fi driver (b43).
- The old Ubuntu ran two Minecraft servers. Their world folders are backed up,
  checksum-verified, in `~/Documents/senior_year/imac-minecraft-backup/`.
- **Mining since 2026-11-09:** systemd unit `jcoin-node` (enabled) runs
  `/usr/local/bin/jcoin-node` (release build of the uncommitted working tree,
  copied to `~/jcoin` on the iMac) with `--data-dir /var/lib/jcoin`, P2P on
  `0.0.0.0:8333`, REST on `127.0.0.1:3000`, rewards to the key `imac` in
  `demo/keys/`. It joined the persisted chain at height 151 (synced first,
  then mining enabled, so it could not fork from genesis).
- **Reference point for difficulty analysis:** the iMac mined its first block
  at height **152** (2026-11-09 22:30:56 local, 04:30:56 UTC); it synced to
  151 first. Heights 1-151 had only the three laptop miners (debug builds).
  `demo/miners.py` attributes every block on the best chain to its coinbase
  key (noah, jon, holden, imac), per epoch and since height 152. Jonathan's
  working assumption (2026-11-09): the jump in mining power from the iMac
  explains the unstable block times; recheck with `miners.py` after the chain
  has run for a while, before changing the retarget rule.
- **Second reference point:** the laptop miners (noah, jon, holden) switched
  from debug to release builds at height **180** (the chain was at 179 when
  they restarted, 2026-11-09 about 23:27 local). `demo/start.sh` and
  `demo/txgen.sh` now use `target/release`. So for difficulty analysis:
  1-151 three debug miners; 152-179 plus the iMac (release); 180 onward all
  four miners on release builds. Epoch 180-191 is the first clean epoch.
- **Decision (Jonathan, 2026-11-09):** GPUs are eventually meant to be the main
  miners. Proof of work stays GPU-friendly (SHA-256 now; a GPU-oriented
  algorithm is possible later); a GPU miner has to be written. The retarget
  rule still needs to react faster (per-block LWMA was recommended), pending
  the data review.
- **Data review (2026-11-10):** with the miners unchanged from 180, the rule
  converged as its formula predicts: 180-191 58 s/block (4x clamp), 192-203
  234 s, 204-215 296 s. The working assumption holds; no bias in the formula
  was found. The remaining problem is speed: a large change in mining power
  takes several epochs to absorb because of the 4x clamp and the one-epoch lag.
- **Third reference point:** the GPU miner (`gpu-miner/`, Intel Iris Xe, about
  140 MH/s, key `gpu`, started with `GPU=1 demo/start.sh` against noah) mined
  its first block at height **216** (2026-11-10 about 01:40 local). The CPU
  miners and the iMac keep mining. The GPU is about 15x the CPU network, so
  epochs from 216 run far below 300 s/block until the 4x steps catch up.
- The laptop firewall (ufw) blocks inbound LAN connections, so the laptop
  dials the iMac: `demo/start.sh` gives noah `--peer $IMAC_PEER`
  (default `10.0.0.88:8333`). If the iMac's DHCP address changes, set
  `IMAC_PEER` or reserve the address in the router.
- Hardware fixes on this iMac (it froze once, idle, about 12 min after boot):
  - GPU runtime suspend is disabled (`/usr/local/sbin/screen-off` writes `on`
    to `0000:00:02.0/power/control`). i915 runtime PM tried to enter PC8 every
    23 s and logged a WARN each time ("PCH PWM1 enabled"); this is the likely
    cause of the freeze.
  - ACPI GPE 06 is masked (`acpi_mask_gpe=0x06` in
    `/etc/default/grub.d/90-jcoin-miner.cfg`); it fired about 25,000 times
    per second and used about 80% of one core.
  - Hardware watchdog: `RuntimeWatchdogSec=60s`
    (`/etc/systemd/system.conf.d/10-watchdog.conf`) reboots it if it hangs.
  - Serial console removed from the kernel command line and
    `serial-getty@ttyS0` masked (the iMac has no serial port).
- The miner uses one CPU core (the node mines on one thread); the iMac
  has four.

The demo network (relay + Noah/Jon/Holden miners + `txgen.sh`) was left
running on the Mint laptop when the session ended. `demo/stop.sh` stops it;
`demo/start.sh` resumes the same chain.

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
- **Amounts + reward halving** (`src/amount.rs`, `Params::block_reward` in
  `src/consensus.rs`): amounts are `u64` base units, 1 JEF = 100,000,000. The block
  subsidy is 50 JEF at height 1 and halves every 32,000 blocks (about 111 days);
  `Chain::context_for_child` supplies the reward to validation, so a coinbase that
  claims a pre-halving reward is rejected. Supply caps just under 3,200,000 JEF; the
  reward reaches 0 at about 10 years, but ~89% is minted in year 1.
  The wallet parses decimal JEF (`12.5`, max 8 decimal places).
- **Difficulty + timestamps**: epoch-based retarget toward `TARGET_BLOCK_TIME`
  (300 s). Difficulty is constant within a `RETARGET_WINDOW` (12) epoch,
  recomputed only at a boundary from that epoch's span, clamped to 4x/step, never
  easier than `pow_limit`. Median-time-past (last 11) is a consensus lower bound
  on timestamps (in `chain.accept`); a 2-hour future bound is a local admission
  rule in the node core (never in deterministic validation, so replay is stable).
- **Chain + fork choice** (`src/chain.rs`): hash-keyed block store; each block
  keeps height, cumulative work (`U256`), and resulting state; canonical tip is
  greatest cumulative work; reorg is a pointer move.
- **Chain sync**: `NEW_BLOCK` gossip; a behind node sends a block locator via
  `GET_BLOCKS` and receives `BLOCKS` (initial download and orphan catch-up).
- **P2P** (`src/p2p/`): the JeffyCoin protocol — `JEFY` frame codec
  (`frame`), messages (`message`), multiaddr `ADDR` encoding (`addr`), the
  `PeerLink` seam (`link`), handshake + heartbeat + run loop (`peer`), session
  registry / address book / dial requests (`network`) — runs unchanged on
  libp2p (`host`): one `/jeffycoin/2.0.0` stream per peer over TCP or a relay
  circuit, Noise-encrypted, Yamux-multiplexed, with peer ids from an Ed25519
  key in `<data-dir>/node_key`. NAT traversal: circuit relay v2, rendezvous
  discovery (namespace `jeffycoin`) and DCUtR hole punching. The `relay`
  binary (`relay_host`) is the relay + rendezvous node. Deployment:
  `docs/DEPLOY-RELAY.md` + `deploy/`; build lean with
  `--no-default-features --bin relay`.
- **Persistence** (`src/store.rs`): append-only block log; state is replayed on
  startup, so blocks are the single source of truth. Enable with `--data-dir`.
- **Node** (`src/bin/node.rs`): node-core task + axum REST + libp2p host
  (`--p2p-addr`, `--peer`, `--relay`, `--no-listen`, `--key-file`) + optional
  miner. Full nonce/balance validation on
  submit (ADR 000). `/chain` exposes height, tip, and the difficulty target.
- **Wallet** (`src/bin/wallet.rs`): clap CLI; `key new/show`, `transaction
  new/status`; fetches nonce+balance and refuses to overspend.

### Tests

`cargo test --lib` (85). Highlights: frame/message/addr round-trips;
handshake + self-connection; gossip over TCP and over libp2p (direct, and
relay-only nodes that find each other through rendezvous); late-joiner
sync; work-based reorg; retarget hardening/cap; median-time-past rejection; an
adversarial block battery (bad PoW, wrong target, bad tx hash, bad body, bad
signature, overspend, unknown version or type, non-canonical coinbase); the
mempool (conflicts, cap, re-admission); torn block-log repair; address-book and
dial caps; queued transactions confirming in order; the halving enforced by
`chain.accept` across a real boundary. Tests run the real code with the
`REGTEST` parameter set (easy PoW, zero genesis timestamp), so they mine in
milliseconds.

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

# Relay (for nodes behind NAT):
cargo run --bin relay -- --addr 127.0.0.1:9000 --key-file ./relay_key
#   It prints its address, /ip4/127.0.0.1/tcp/9000/p2p/<relay id>
#   (or: relay ... --show-address). Start nodes with --relay <that address>;
#   add --no-listen for a node reachable only through the relay.

# Wallet
cargo run --bin wallet -- key new alice
cargo run --bin wallet -- --node http://127.0.0.1:3000 \
  transaction new alice <recipient-hex-64> 100
curl -s localhost:3000/chain   # height, tip, difficulty target
```

## Live demo network (persisted)

`demo/` in the repo holds the one persisted demo chain. **All live testing
runs on it; do not start fresh chains for live tests.** `start.sh` starts a
relay and three mining nodes on the existing `data-*/` dirs: Noah and Jon
listen directly, Holden is relay-only (`--no-listen`) and finds them through
rendezvous. `txgen.sh` sends random transfers (logged to `txgen.log`),
`status.sh` prints heights and balances, `stop.sh` stops everything (the chain
stays). Chain data, keys, and logs are git-ignored. The miners keep three
cores busy while it runs.

## Known limitations / not built

See `KNOWN-ISSUES.md` for the full list with severities, including the
security-relevant vulnerabilities (unbounded fork storage, no peer banning,
time-warp, no finality). Summary:

- **Reorg mempool re-add**: transactions dropped by a reorg are not re-queued
  automatically. Resubmitting one now works (the mempool, not the seen cache,
  decides admission).
- **Relay**: one relay per node; the address book is not persisted.
- **Throughput**: a v1 block holds one transfer, so the network confirms
  about 12 transactions per hour at 5-minute blocks.
- **No misbehaviour scoring/banning** (the notes mention a blacklist).
- No block/consensus-version signaling.
- **Persistence** stores blocks only and never compacts the log.
- `MAINNET.pow_limit` is machine/build-specific (calibrated from a debug-build hash rate).

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
- Consensus values live in `consensus::Params`. Tests use `REGTEST` (or a
  modified copy, for example a 4-block halving interval); the real network is
  `MAINNET`. Regtest and mainnet have different genesis blocks, so their nodes
  reject each other's blocks.
- **Do not run `cargo fmt`.** The repo is not rustfmt-formatted; it rewrites
  hundreds of unrelated lines.

## Suggested next steps

1. Decide block capacity (transfers per block): the throughput limit above.
2. The audit's medium findings M1/M3/M4 (one gossip component driven by core
   events, typed errors, a `NodeHandle`), then peer scoring (V2).
3. Fork-store pruning (V1), reorg mempool re-add (O1).
4. Several relays per node and a persisted address book.
5. Push to GitHub and open the work for the friend nodes.
