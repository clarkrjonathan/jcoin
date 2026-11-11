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

### Log attributes that make the logs cheaper to read

- Consensus: no
- Found: 2026-11-11, while building the log reader (`tools/sentry-logs`) and
  the `sentry-logs` skill. The reader has to fetch the full text of a line to
  learn which peer it is about, why a dial failed, or what height a node is
  at. One failed-dial line is 300 to 800 characters, and a node logs dozens on
  each start.
- Planned change: put those facts in attributes, so a query can ask for the
  attributes and leave the text out.
  - `run`: a short random id made at each app start. Selects one run of one
    node without a time window.
  - `peer`: the 6 characters after `12D3KooW` of the other node a line is
    about (dials, connections, hole punches, streams).
  - `reason`: a short class for a failure, for example `timeout`, `refused`,
    `no_reservation`, `address_in_use`, `already_dialing`, `handshake`.
  - `via`: `relay` or `direct`, for dials and connections.
  - `height`: the block height a line is about (blocks, replay, sync).
- Planned change: a `status` line once a minute with the node's state in
  attributes (`height`, `peers`, `reservation` yes/no, `mining` yes/no,
  `hash_rate`, `pending`). One row then answers "what state is this node in"
  without reading its history.
- Planned change: log a sent transfer (`transfer_sent`, with its id, amount,
  and nonce) and its confirmation (`transfer_confirmed`). The log has no
  record of sends today.
- Planned change: send failed dials to addresses that can never be reached
  (loopback, link-local) at a lower level or not at all. See "Log noise from
  addresses that cannot be reached".

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

- Deploy the 2.1.1 relay program to the Lightsail relay, so that refused
  reservations appear in its log.
- Deploy a current node to the iMac reference node. It still runs a build with
  the earlier 35-bit genesis and cannot sync with 2.x nodes.
- Add the `SENTRY_AUTH_TOKEN` repository secret, so that crash reports show
  function names and line numbers.
