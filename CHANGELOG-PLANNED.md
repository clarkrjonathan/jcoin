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

## 2.2.0 (consensus change)

### The difficulty rule compounds and stalls the chain

- Status: planned. Urgent: the chain is stalled.
- Consensus: **yes**. Every node must update. The fix activates at a block
  height through the version-by-height rule (`consensus_version`), or the
  chain is restarted.
- Found: 2026-11-11. The chain reached height 69 at 05:33 UTC and produced no
  block for more than 8 hours, with about 2.1 GH/s mining. Difficulty went
  from 28 to 44.8 leading zero bits (about 110,000 times harder) in 9 blocks.
  Two GPUs joining explains about 4 bits, not 17.
- Cause: the rule in `chain.rs` `target_for_child` is not an LWMA. It
  multiplies the **parent's** target by (weighted average solve time of the
  last 60 blocks / 300 s), every block. The 60-block average changes slowly,
  so while it is below 300 s the difficulty is multiplied again on every
  block, by up to 4. The first 60 blocks were mined at the 28-bit floor in a
  second or two each, so the average stayed under 300 s for 12 more blocks and
  the difficulty rose 4x per block: 30, 32, 34, 36, 38, 40, 42, 43.6, 44.8
  bits. Blocks 63 to 69 took 128 s to 5,672 s each, far over the target, and
  the difficulty still rose.
- It also does not settle. Once slow blocks fill the window the same rule
  divides the difficulty by up to 4 per block until it hits the floor, then
  the cycle repeats. The regtest demo stalled at height 99 the same way.
- Contributing: blocks 1 to 60 use the floor with no adjustment
  (`height <= lwma_window`), and the 28-bit floor is about 2 seconds of work
  for a laptop GPU, so the window filled with near-zero solve times.
- Planned change:
  - Use the real LWMA: next target = (average of the **targets** of the last
    N blocks) x (weighted sum of their solve times) / (N(N+1)/2 x 300 s). The
    average target anchors the result, so it does not compound.
  - Adjust from the first blocks, with a window that grows up to N, in place
    of 60 blocks at the floor.
  - Tests that simulate a chain: a miner far faster than the floor, a hash
    rate step up and down, and a miner leaving. Each must settle near 300 s
    and never run away. The current tests only check one adjustment step.
- Open points:
  - Activate at a height on the current chain (balances are kept; blocks 1 to
    69 stay valid under the old rule) or restart the chain. The next block
    under the old rule needs about 45 bits, so the activation height would be
    70: the first block mined by an updated node.
  - The floor value.

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

## 2.1.4

NOTE THIS IS A USER ADDED FEEL FREE TO MODIFY THE FORMAT TO FIT THE REST WHEN YOU NEXT TOUCH THIS

I would like to add a block tracker and average block time so I can see the health of the network

Additionally add some projections for time assuming this nodes hash rate is the only one hashing, eg. what is the current nodes expected time to block?

Also look into the node gossip protocol for discovery, I would like for even indirect relay connections to be gossipped if possible

And what is the gpu thing actually doing? Because when I go to 100% it spikes like 10x, but everything below 100% is practically the same

Include device label in the node connections

### Consensus backlog
ANOTHER IMPORTANT NOTE, I WOULD LIKE TO BACKLOG THE FOLLOWING MAKE A SEPERATE STRUCTURE FOR CONESENSUS BACKLOG
I would like to readdress the consensus for the limit to target scaling, my friends had their gpus on and then turned them off and completely stalled the block chain because my gpu is less powerful than theirs and couldn't keep up
Additionally I believe the singular block model may have made it even worse, even with all gpus contributing that would be a expected block time of 1.6 hours so something clearly went wrong. This algorithm needs more work
is there a **safe** way to implement some sort of 20 minute fall back where we are able to revert to a lower target or just have some other fall back system for this to prevent complete stalls for hours

Imediate consensus needs to be made to change away from singular block adjustment I think. The problem is a particularly lucky block shifts target, but if we expand to larger epoch it means a drop in network compute power means we have to fight for longer. Is there a better solution?

Also this may fit somewhere else but maybe a large conflicting fork with much higher pow strongly prompts for a client upgrade? If this is a good idea it should be pushed very soon

## 2.1.5

saved addresses for outgoing transactions
More detailed block view for the developer view, eg full block view just expands out as you click through it

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
