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

## 2.1.2

### Share pending transfers with a peer that connects later

- Status: planned
- Consensus: no
- Found: 2026-11-11. A transfer was sent while two nodes were connected. A
  third node connected afterwards and mined the next block. The block did not
  include the transfer, and the transfer stayed pending on the sender.
- Cause: a node announces a transfer to its peers once, when it first accepts
  it (`NewTransaction`). The protocol has no message that gives a newly
  connected peer the transfers that are already pending. A node that joins
  later never learns of them, so the blocks it mines leave them out.
- Planned change: when a session with a peer starts, send that peer the
  pending transfers (the mempool). Old nodes accept these as ordinary
  `NewTransaction` messages, so old and new versions work together.
- Open point: whether to also announce pending transfers again on a timer, for
  a peer that was connected but missed the first announcement.

### A limit on how much of the GPU mining uses

- Status: planned
- Consensus: no
- Problem: mining runs the GPU without pause. The computer is slow to use
  while it mines, and the GPU runs hot and loud. The only control is to stop
  mining.
- Planned change: a setting in the app's Mining section that limits GPU use,
  as a share of full power (for example 25%, 50%, 75%, 100%). The mining loop
  pauses between GPU dispatches so that the GPU is busy for about that share
  of the time. The setting is remembered between runs and can be changed while
  mining. The command-line miner gets the same limit as an option.
- Effect on mining: the hash rate falls in proportion to the limit, so blocks
  are found less often.
- Open point: whether the limit should also lower itself while the computer is
  in use and rise when it is idle.

## Unscheduled

Problems that have been observed and have no version yet.

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
