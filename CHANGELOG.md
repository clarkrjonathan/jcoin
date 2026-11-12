# Changelog

What changed in each released version. A version is a `v*` tag on `main`; the
release workflow builds it. Dates are UTC.

Changes that are planned but not released are in
[`planned.toml`](planned.toml); `tools/planned` lists them by version.

Each entry says whether it affects **consensus**. A consensus change makes old
and new nodes disagree on which blocks are valid, so every node must update.
All other changes are compatible with older nodes on the same chain.

## 2.2.6 (2026-11-12)

A patch. No consensus change. The program code is the same as 2.2.5; only
the Linux build changed.

### Fixed

- The 2.2.4 and 2.2.5 Linux programs did not start on Ubuntu 24.04: they
  needed glibc 2.43 ("version `GLIBC_2.43' not found"). The release build
  used GitHub's `ubuntu-latest`, which moved to Ubuntu 26.04. Linux is now
  built on Ubuntu 24.04, and the build fails if a Linux program needs a
  glibc newer than 2.39. Windows was not affected.

A Linux install that updated to 2.2.4 or 2.2.5 in the app no longer starts.
Install 2.2.6 by hand: download `JeffyCoin-linux-x86_64` from the release,
run it, and install from its window.

Consensus: no.

## 2.2.5 (2026-11-12)

A patch. No consensus change.

### Fixed

- On Linux, the in-app update started the old program again instead of the
  new one. The updater renames the running program to `<program>.old`, and
  on Linux the running program's own path follows that rename. It now
  restarts the path it read before the swap. Windows was not affected.

The update to 2.2.5 is still done by the old updater, so on Linux it starts
the old program once more: close the app and open it again from the menu.
Updates after 2.2.5 restart the new program.

Not tested: a real update and restart (it needs a release newer than the
one updating). Not tested on Windows; the Windows path is unchanged.

Consensus: no.

## 2.2.4 (2026-11-12)

A patch. No consensus change: it runs on the same chain as 2.2.0 and
connects and syncs with 2.2.0 to 2.2.3. Decisions in
`docs/adr/0005-reliability-patch-2-2-4.md`.

### Fixed

- Pending transfers were lost when the app was closed. The node now keeps
  them in `pending.dat` in the data folder and has them pending again after
  a restart. Ones that are no longer valid are dropped.
- A reorg lost the transfers of the blocks it abandoned. They now go back
  into the queue and confirm in a later block.
- A transfer that arrived before the one it follows (nonce `n + 1` before
  `n`) was refused. It is now held for up to 10 minutes and admitted when
  the gap fills.
- A node kept every fork block it was sent, each with a copy of all account
  balances, so its memory could grow without limit.
- Two nodes could keep up to three direct connections to each other. Extra
  ones are now closed.

### Changed

- A node follows a reorg at most 288 blocks (about a day) deep. Side
  branches that fork deeper are dropped, and a block that forks deeper is
  refused and logged.
- The sent list keeps the last 100 transfers across restarts (`sent.txt`)
  and is now called "Sent".

### Checked

- `tools/check --gpu`: the mine-and-send test passed on a private chain.
  The test program then crashed on exit (known issue O9; tests only).
- Direct connections: a local relay and two nodes on this computer.

Not tested on Windows.

Consensus: no.

## 2.2.3 (2026-11-11)

A patch. No consensus change: it runs on the same chain as 2.2.0 and
connects and syncs with 2.2.0 to 2.2.2.

### Added

- The Network section shows how long ago the last block came, and the
  average time between the last 30 blocks next to the 5-minute target. The
  "last block" time turns amber after four times the target.
- A "Recent blocks" table (Network section, folded by default): height, how
  long ago, time after the block before, difficulty in bits, and the miner
  ("you" for your own keys).
- While mining, the Mining section shows the expected time for this computer
  to find a block if it mined alone: the current difficulty divided by its
  hash rate. It is an average; a block can take much less or much more.
- The node has a new REST route, `GET /chain/recent?count=N` (last N blocks,
  gaps, average), and `GET /chain` also gives the next block's expected
  hashes and the target block time.

### Changed

- Two nodes that reach each other through the relay now try each other's
  direct addresses (for example, on the same home network). If a direct
  connection opens, from that or from a hole punch, the relayed connection
  is closed and the node exchanges blocks over the direct one. Before, two
  computers on one network talked through the relay.

### Fixed

- Connections that another node opened through the relay were shown as
  "direct" in the connections list.
- If opening a session to a connected peer failed, it was not tried again.
  It is now tried again after one second.

### Checked

- The GPU use limit (fixed in 2.2.0) was measured through the app's mining
  loop: 50% gives about half the hash rate and 25% about a quarter.
  `tools/check --gpu` repeats the measurement.

Not tested on Windows. The direct-connection change was tested with a local
relay and two nodes on this computer, not on two computers on one network.

Consensus: no.

## 2.2.2 (2026-11-11)

A patch. No consensus change: it runs on the same chain as 2.2.0. Nodes on
2.2.2 and 2.2.1 still connect and sync with each other.

### Added

- The connections list (Developer section) shows each peer's friendly name
  beside its peer id. A node now sends its device label to the nodes it
  connects to, after the handshake. The label is cosmetic and is visible to
  every peer connected to.
- On first run, the app asks what to call this computer, with a Skip button.
  The name can still be changed later in the Developer section.
- The update screen says why "Install and restart" is unavailable, and offers
  "Stop mining and install" when mining is the only thing in the way.
- The log records more of what a node does: a once-a-minute status line
  (height, peers, mining, pending, balance, and more), transfers sent, blocks
  received, syncs, reorgs, dropped pending transfers, and the update and
  install steps. These go to the log file and to the developer's reports.

### Changed

- The logs carry the facts worth querying (which peer, why a dial failed,
  relay or direct, which height) as fields, so a question can be answered
  without reading each line's full text.
- A node no longer stores, shares, or dials loopback and link-local addresses
  of other machines, which removes most of the failed-dial noise. Ordinary
  local-network and relayed addresses are unchanged. A repeated failed dial is
  kept in the local log file but reported once per run.

Consensus: no.

Not tested on Windows; this version's build is the first Windows run.

## 2.2.1 (2026-11-11)

A small patch. No consensus change: it runs on the same chain as 2.2.0.

### Changed

- The example in the "Label for this computer" field (Developer → Reporting)
  now reads "Jons Desktop".

Consensus: no.

Not tested on Windows; this version's build is the first Windows run.

## 2.2.0 (2026-11-11)

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
