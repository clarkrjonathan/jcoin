# Changelog

What changed in each released version. A version is a `v*` tag on `main`; the
release workflow builds it. Dates are UTC.

Changes that are planned but not released are in
[`CHANGELOG-PLANNED.md`](CHANGELOG-PLANNED.md). When a version is released, its
section moves from that file to the top of this one.

Each entry says whether it affects **consensus**. A consensus change makes old
and new nodes disagree on which blocks are valid, so every node must update.
All other changes are compatible with older nodes on the same chain.

## 2.2.0 (unreleased)

**The chain restarts.** This release has a new genesis (2026-11-11 19:05 UTC)
and block version 3. Blocks and coins of the earlier chain are not carried
over. Key files stay valid. Every node must update to take part; 2.1.2 and
earlier stay on the old chain.

### Changed (consensus)

- **Difficulty rule replaced.** The earlier rule scaled the previous block's
  difficulty by a 60-block average on every block. That compounds: on
  2026-11-11 the difficulty rose from 28 to 44.8 bits in nine blocks and the
  chain stalled at height 69. The new rule (`chain::lwma_next_target`) scales
  the **average** difficulty of the last 30 blocks by their weighted average
  solve time, so it cannot compound. It adjusts from the first block. One
  block's solve time counts for at most 30 minutes.
- **Stall fallback.** If a block's timestamp is 20 minutes after its
  parent's, its difficulty is halved, and halved again for every 10 minutes
  after that, down to the starting difficulty. When hash power leaves, the
  next block arrives in about an hour at worst, in place of hours or days. An
  eased block carries less work, so a block at normal difficulty at the same
  height wins.
- **Timestamps at most 10 minutes ahead.** A node accepts a block only if its
  timestamp is no more than 10 minutes ahead of the node's clock (it was 2
  hours). This keeps a miner from claiming the stall fallback with a false
  timestamp. A computer whose clock is more than 10 minutes slow will refuse
  current blocks.
- **Block version 3.** The version rises with every consensus change and is
  never reused.
- Starting difficulty stays at 28 leading zero bits.

### Added

- **A banner when the network has moved to newer rules.** A node counts the
  blocks it refuses that carry a newer block version and real proof of work,
  and compares their work over 24 hours with its own chain's
  (`GET /network/signal`). The app shows a notice when any are seen, and a
  stronger banner when they carry at least as much work as its own chain. The
  banner can be hidden. Nothing is stopped: there are no required updates.
- The app reads the release list every 30 minutes while open, not only at
  startup.
- Blocks of the earlier chain in the data folder are kept as
  `blocks-previous-chain.dat`, and the new chain starts empty.

### Fixed

- **The GPU use limit had almost no effect below 100%.** The rest between GPU
  runs happened in steps of 100 ms, so every setting under 100% rested at
  least 100 ms per run. It now rests for the computed time.

### Tested

- Simulated chains, 20 random seeds each: a miner 4,096 times faster than the
  starting difficulty; hash power stepping up 16 times and back; hash power
  falling to 1/15 and 1/100. Block time settles near 300 s, the difficulty
  never passes equilibrium by more than about 3 times, and after a 100-fold
  drop the first block comes within 3 hours and ten blocks within 12 on every
  seed.
- Not run on Windows before this release.

## 2.1.2 (2026-11-11)

### Fixed

- **A transfer could be left out of blocks mined by a node that connected
  later.** A node announced a transfer to its peers once. A node that
  connected afterwards, or was still syncing, never learned of it and mined
  blocks without it. Each session now sends its peer the pending transfers
  when it starts and again every 60 seconds. Older nodes accept these as
  ordinary announcements.
- An empty `XDG_DATA_HOME` or `APPDATA` variable made the app put its data
  folder in the current directory. An empty variable now counts as unset.

### Added

- **GPU use limit.** A slider in the app's Mining section (10% to 100%) limits
  how much of the GPU mining uses; the hash rate falls in proportion. It can
  be changed while mining and is remembered. The command-line miner has
  `--limit <percent>`, which also applies to `--bench`.
- **Install for the current user.** A downloaded copy offers to install: it
  copies itself to a fixed place under a short name
  (`%LOCALAPPDATA%\Programs\JeffyCoin\JeffyCoin.exe`, `~/.local/bin/jeffycoin`),
  adds a "JeffyCoin" entry to the Start menu or application menu, and starts
  the installed copy. The download is left in place. No administrator rights
  are needed.
  - Updates apply only to the installed copy.
  - A downloaded copy can open the installed app, or replace it when the
    versions differ.
  - The installed copy has "Uninstall", which keeps the data folder.
  - An "Open" button next to the data folder path.

### Known limits

- The install has not been run on Windows before this release.
- The Windows Start menu entry shows a generic icon, and the app is not listed
  under "Installed apps".
- A downloaded copy and the installed copy can run at the same time on the
  same data folder.

Consensus: no change.

## 2.1.1 (2026-11-11)

### Fixed

- **A node could stay unreachable for a whole run.** If the first attempt to
  reserve a slot on the relay failed, the node never tried again. It could dial
  out, but no other node could find it or dial it. The node now connects to the
  relay first, requests the reservation on that connection, and requests it
  again every 10 seconds while it does not hold one. Seen on Windows.
- **Outgoing connections failed on Windows** with "Only one usage of each
  socket address" (os error 10048). On Windows, outgoing connections now use a
  new port instead of the listen port. A direct (hole-punched) connection from
  a Windows node is not expected to work; such nodes stay on the relay.

### Added

- **Reporting to Sentry.** The desktop app sends every log line and crash
  report. Each line carries attributes for sorting: `component`, `event`,
  `device`, `install_id`, `peer_id`, `network`, `address`, `gpu`. The Developer
  section says that logs are sent and has a field to label the computer.
  `JCOIN_APP_NO_TELEMETRY=1` turns reporting off.
- The node logs when it loses its relay reservation.
- The relay logs refused reservations and refused circuits.
- The release workflow uploads the app's debug files to Sentry when the
  `SENTRY_AUTH_TOKEN` repository secret is set.

Consensus: no change.

## 2.1.0 (2026-11-11)

### Added

- **Desktop app** (`JeffyCoin-windows-x86_64.exe`, `JeffyCoin-linux-x86_64`):
  the node, the wallet, and the GPU miner in one window. It creates a key on
  first run and connects through the public relay.
  - Keys: create, switch, import, and back up.
  - Mining on a chosen GPU, with the live hash rate.
  - Sending, with "waiting for a block" and "confirmed" status.
  - Developer section: open connections, blocks mined, the log, and "Copy
    diagnostics".
  - A log file, `jeffycoin.log`, in the data folder.
  - Updates: the app lists the GitHub releases, verifies the SHA-256 of the
    chosen one, replaces its own program file, and restarts.
- `GET /peers` on the node: open connections and the node's own addresses.
- `GET /chain` reports the peer count.
- The mining submit reply carries the block's reward, transfer count, target,
  and timestamp.

### Changed

- The node's startup and REST server moved into the library
  (`jcoin::service`); the `node` program is a front end for it. Key files and
  signing moved into `jcoin::wallet`. The GPU miner is a library plus a
  program.
- Key names are limited to letters, digits, `-`, and `_`.
- The release workflow builds only for a `v*` tag whose commit is on `main`
  and whose name matches the app version.

Consensus: no change.

## 2.0.0 (2026-11-11)

The consensus v2 chain. It replaces the v1 demo chain; v1 and v2 nodes reject
each other's blocks. See `docs/adr/0001-restart-consensus-v2.md`.

### Changed (consensus)

- **Difficulty** adjusts every block from a linearly weighted moving average
  (LWMA) of the last 60 block times, in place of an adjustment once per
  12-block epoch.
- **Blocks** carry up to 64 transfers, in place of at most one.
- **Starting difficulty** is 28 leading zero bits, in place of 20.
- **Block version** is 2, checked against a version chosen by height, so a
  later rule change can activate at a height without a new chain.
- **Genesis**: a new marker and the timestamp 2026-11-11 00:52 UTC.

### Added

- Peer-to-peer transport on libp2p: relay circuits, rendezvous discovery, and
  hole punching. Nodes listen on IPv4 and IPv6.
- GPU miner (`jcoin-gpu-miner`), for any GPU through wgpu.
- Release builds for Linux and Windows with SHA-256 checksum lists.

### Known limit

- The node's own miner and the mining endpoints put at most one transfer in a
  block, although the rules allow 64.
