# Planned changes

Changes that are decided or proposed but not released. Released changes are in
[`CHANGELOG.md`](CHANGELOG.md).

How to use this file:

- Add an entry when a problem or a wanted change is found, before any code is
  written. State the problem as observed, then the planned change.
- Put the entry under the version it is planned for, or under "Unscheduled" if
  no version is chosen.
- Mark each entry **Consensus: yes** or **Consensus: no**. A consensus change
  needs every node to update and must be planned around an activation height.
- When the version is released, move its section to the top of
  `CHANGELOG.md`, reworded as what changed, and delete it here.

Status values: `planned` (agreed, not started), `in progress`, `done` (merged
to `main`, waiting for the release).

## 2.1.3

Theme: make the Sentry logs answer a question in one query. Since 2.1.1 the
app sends every log line, and `tools/sentry-logs` and the `sentry-logs` skill
read them. Reading them on 2026-11-11 showed what is missing. None of this
changes consensus.

### Name the computer on first run

- Status: planned
- Consensus: no
- Problem: no node had a device label, because the field is inside the
  Developer section. Nodes had to be told apart by peer id and operating
  system.
- Planned change: on first run, ask "What should this computer be called?"
  with a short text field and a "Skip" button. The answer is the `device`
  label. The field in the Developer section stays, for changing it.

### A status line once a minute

- Status: planned
- Consensus: no
- Problem: to learn whether a node is connected, synced, and mining, its whole
  history has to be read.
- Planned change: log a `status` line every minute with the state in
  attributes: `height`, `peers`, `reservation` (yes or no), `mining` (yes or
  no), `hash_rate`, `gpu_limit`, `pending` (count of pending transfers),
  `balance`. One row then answers "what state is this node in".
- The line's text stays short, so the log file is not flooded.

### Log the events that are missing

- Status: planned
- Consensus: no
- Problem: the log has no record of several things that had to be worked out
  by hand on 2026-11-11.
- Planned change: add lines, each with its own `event`:
  - `transfer_sent` (id, amount, fee, nonce, recipient) and
    `transfer_confirmed` (id, height).
  - `transfer_received` when the balance of the key in use rises by a
    transfer (not by a mined block).
  - `block_received` (height, from which peer) and `sync_done` (height, block
    count) when a node catches up from a peer.
  - `reorg` (old tip, new tip, depth).
  - `update_started`, `update_installed`, `update_failed` (versions, reason),
    and `installed` / `uninstalled` from the install flow.
  - `pending_dropped` when a pending transfer leaves the mempool without being
    confirmed.

### Stop sending lines that say nothing

- Status: planned
- Consensus: no
- Problem: about half of a node's lines are failed dials to addresses that can
  never be reached (loopback, link-local, addresses of old test nodes) and
  "already connected or dialing" notices.
- Planned change:
  - Do not pass on or dial loopback and link-local addresses of other
    machines. This removes the cause, not only the lines.
  - Do not log "already connected or dialing"; it is not a failure.
  - Send a failed dial to Sentry only the first time for each peer in a run;
    keep every one in the local log file.

### Attributes that make a line cheap to read

- Status: planned
- Consensus: no
- Problem: the reader has to fetch a line's full text to learn which peer it
  is about, why something failed, or what height a node is at. One failed-dial
  line is 300 to 800 characters.
- Planned change: put those facts in attributes, so a query can ask for the
  attributes and leave the text out.
  - `run`: a short random id made at each app start. Selects one run of one
    node without a time window.
  - `peer`: the 6 characters after `12D3KooW` of the other node a line is
    about (dials, connections, hole punches, streams).
  - `reason`: a short class for a failure: `timeout`, `refused`,
    `no_reservation`, `address_in_use`, `handshake`, `other`.
  - `via`: `relay` or `direct`, for dials and connections.
  - `height`: the block height a line is about.
- Where: `telemetry::classify` in the app takes these from the line's text
  today. Cleaner is for the node to log structured fields; decide when
  building.

### Reader and skill follow-up

- Status: planned
- Consensus: no
- After the changes above, update `tools/sentry-logs` and the `sentry-logs`
  skill:
  - `sentry-logs status NODE`: the latest `status` row for a node (or for all
    nodes), as one line each.
  - `sentry-logs peers NODE`: who a node connected to, by `peer` and `via`,
    without fetching text.
  - `timeline` and `lines` use `run` to show the latest run by default.
  - Verify the reader's request to Sentry with a real token. It has only been
    tested without one.

## 2.3

### Graphical overhaul of the desktop app

- Status: planned
- Consensus: no
- Problem: the app is one long page that shows everything to everyone. A
  person who only holds coins sees mining and developer controls; a miner has
  no view of what the GPU is doing.
- Planned change: a redesigned window with modes, so each kind of user sees
  what they need. The working list of modes:
  - **Miner**: mining controls and GPU statistics.
  - **Wallet**: balance, keys, sending, and receiving.
  - **Developer**: connections, blocks, the log, and diagnostics.
- GPU statistics to show: hash rate over time, the GPU use limit, blocks
  found, and, where the system reports them, GPU temperature, power, and load.
- Open points:
  - The exact set of modes and their names.
  - Which GPU figures can be read on each system. wgpu does not report
    temperature, power, or load; they need a separate source for each GPU
    maker and operating system.
  - Whether the GPU use limit lowers itself while the computer is in use
    (carried over from 2.1.2).

## Unscheduled

### Pending transfers are lost when the node restarts

- Consensus: no
- Found: 2026-11-11. Pending transfers are kept in memory only. When the
  sender's app was closed with two transfers pending, its node forgot them.
  They can still confirm if another node holds them; otherwise they must be
  sent again. The app's "Sent this session" list is also lost on restart, so
  the sender cannot see which transfers are affected.
- Possible change: store pending transfers in the data folder and reload them
  at startup; keep the sent list across restarts.

Problems that have been observed and have no version yet.

### Two copies of the app can run on the same data folder

- Consensus: no
- Nothing stops a second copy of the app from starting while one is running.
  Both then write to the same chain file and key folder. Since 2.1.2 a
  downloaded copy and the installed copy make this easier to do by accident.
- Possible change: a lock in the data folder; a second copy brings the first
  one's window forward and exits.

### Nodes on the same local network connect through the relay

- Consensus: no
- Found: 2026-11-11. Two nodes on the same LAN (192.168.0.x) connected to each
  other through the relay. Neither tried the other's LAN address.
- Not yet investigated.

### Log noise from addresses that cannot be reached

- Consensus: no
- Found: 2026-11-11. Nodes pass on loopback and private addresses of other
  machines (`127.0.0.1`, `172.17.0.1`, `::1`) and stale records of old test
  nodes. Each node then tries to dial them and logs a failure. Most log lines
  on a fresh start are these failures.
- Possible change: do not pass on or dial loopback addresses, and drop
  addresses of peers that have not been reachable for some time.

### Difficulty overshoots when a fast miner starts a quiet chain

- Consensus: yes, if the adjustment rule changes
- Found: 2026-11-11. A GPU mined the first 63 blocks in about 30 seconds. The
  difficulty rose to about 38 leading zero bits. Blocks then took far longer
  than the 300-second target for the hash power present, and the difficulty
  comes down only a little with each slow block.
- Not yet investigated. Changing the rule is a consensus change.

### The miner puts at most one transfer in a block

- Consensus: no (the rules already allow 64)
- The node's own miner and the mining endpoints select one pending transfer
  for each block. Planned change: select up to 64, highest fee first, each
  sender's transfers in nonce order.

### Hole punching from Windows

- Consensus: no
- Since 2.1.1, a Windows node dials from a new port, because dialing from the
  listen port fails there (os error 10048). The library's hole-punch dials
  still use the listen port, so a direct connection from Windows is expected
  to fail and the node stays on the relay. A fix would be in how the Windows
  sockets are set up.

### The app does not show a transfer that is on its way in

- Consensus: no
- The app shows only the confirmed balance. A recipient sees nothing until the
  transfer is in a block.

## Operations (no release needed)

- Create a Sentry user auth token (scopes `org:read`, `project:read`,
  `event:read`) and save it to `~/.config/jeffycoin/sentry-token`, so
  `tools/sentry-logs` can read the logs.
- Set up Sentry alerts: one for any crash, one for `reservation_lost` with no
  `reservation_ok` after it. Nothing notifies anyone today.
- Check the Sentry plan's log quota against the volume once several nodes run
  all day.
- The relay and the iMac node do not report to Sentry; they log locally only.
  Decide whether the command-line node and the relay should report too.

- Deploy the 2.1.1 relay program to the Lightsail relay, so that refused
  reservations appear in its log.
- Deploy a current node to the iMac reference node. It still runs a build with
  the earlier 35-bit genesis and cannot sync with 2.x nodes.
- Add the `SENTRY_AUTH_TOKEN` repository secret, so that crash reports show
  function names and line numbers.
