# Changelog

What changed in each released version. A version is a `v*` tag on `main`; the
release workflow builds it. Dates are UTC.

Changes that are planned but not released are in
[`CHANGELOG-PLANNED.md`](CHANGELOG-PLANNED.md). When a version is released, its
section moves from that file to the top of this one.

Each entry says whether it affects **consensus**. A consensus change makes old
and new nodes disagree on which blocks are valid, so every node must update.
All other changes are compatible with older nodes on the same chain.

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
