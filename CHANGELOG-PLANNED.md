# Planned changes

Changes that are decided or proposed but not released. Released changes are in
[`CHANGELOG.md`](CHANGELOG.md).

How to use this file:

- Add an entry when a problem or a wanted change is found, before any code is
  written. State the problem as observed, then the planned change.
- Put the entry under the version it is planned for, or under "Unscheduled" if
  no version is chosen.
- Mark each entry **Consensus: yes** or **Consensus: no**. A consensus change
  makes old and new nodes disagree on which blocks are valid.
- A consensus change ships only in a minor version (`2.x.0`), with a higher
  block version, either at an activation height or with a chain restart. Every
  open consensus question is listed in "Consensus backlog" near the end of
  this file, also when its work is planned for a version.
- When a version is released, move its section to the top of `CHANGELOG.md`,
  reworded as what changed, and delete it here.

Status values: `planned` (agreed, not started), `in progress`, `done` (merged
to `main`, waiting for the release), `to design` (the problem is agreed, the
change is not).

The next release is **2.2.0**. Nothing ships as 2.1.3 or later in the 2.1
line.

## 2.2.0: chain restart

A consensus release. The chain restarts with a new genesis and block version
3. Coins mined on the stalled chain are not carried over. Key files stay
valid.

### The difficulty rule compounds and stalls the chain

- Status: planned. Urgent: the chain is stalled.
- Consensus: **yes**
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
- Decided 2026-11-11: restart the chain, in place of activating the fix at a
  height on the stalled chain.
- Open points: see "Consensus backlog" for the questions that are designed
  together with this rule (hash power leaving, a fallback against stalls, the
  window size, the starting difficulty).

### Tell the user when the network has moved on

- Status: planned
- Consensus: no (it is about how a client learns of a consensus change)
- Source: Jonathan's notes, 2026-11-11
- Problem: a client on an older version cannot follow a consensus change. It
  keeps its own chain, refuses every block of the new one, and says so
  nowhere. Its only hint is the line "A newer version is available" in the
  Updates section, checked once at startup. A client left open never checks
  again.
- Decided 2026-11-11: **no required updates.** The app never refuses to run
  or to mine because of its version. A flag on the release page that stops
  old clients would make one party decide consensus. A client on old rules
  that keeps mining is the system working as designed. The app prompts more
  strongly as the evidence grows; it does not force.
- Planned change: take the evidence from the network.
  - **Higher block version.** The block version rises on every consensus
    change and is never reused (see "Consensus backlog"). When a client
    refuses a block because its version is higher than any it knows, and the
    block's header meets the target the header states, that is proof of work
    spent on newer rules. The client can check this without knowing the newer
    rules, for a fork of its own chain and for a chain with another genesis.
  - **Weight by proof of work.** The client adds up the work of those
    headers and compares it with the recent work on its own chain. Any such
    block gives a notice in the Updates section. Once the work is comparable
    to or larger than its own chain's, the app shows a banner at the top of
    the window: newer rules are in use and carry more work than this chain;
    update. The banner has the update button, can be dismissed for the
    session, and never stops mining.
  - **Heavier chain without a higher version.** A refused chain that carries
    much more work but no higher version gets a weaker notice ("a heavier
    chain exists that this version cannot read"). More work alone does not
    say which side is right, and on a small chain one GPU can produce it.
  - **Update check on a timer.** The app checks the release list at startup
    and every 30 minutes, as information for the notices above.
  - **Peers' rule version in the connections list**, so the Developer section
    shows which peers run other rules. Lower priority.
- Not done on purpose: separate discovery for each chain. It would hide the
  new chain's blocks from old clients, and then they would never see the
  signal. Nodes of the old and new chain keep connecting to each other.
- Limit: 2.1.2 and earlier cannot show any of this. For this restart, the
  people running them have to be told to update.

### Old chain data is moved aside

- Status: planned
- Consensus: no
- Problem: 2.2.0 starts a new chain, and the data folder holds the stalled
  chain's `blocks.dat`.
- Planned change: on first start, 2.2.0 renames the old file
  (`blocks-v2-stalled.dat`) and starts the new chain. Key files are not
  touched. Balances start at zero.

## 2.2.1: logs that answer a question in one query

Since 2.1.1 the app sends every log line to Sentry, and `tools/sentry-logs`
and the `sentry-logs` skill read them. Reading them on 2026-11-11 showed what
is missing. None of this changes consensus.

### Name the computer on first run

- Status: planned
- Consensus: no
- Problem: no node had a device label, because the field is inside the
  Developer section. Nodes had to be told apart by peer id and operating
  system.
- Planned change: on first run, ask "What should this computer be called?"
  with a short text field and a "Skip" button. The answer is the `device`
  label. The field in the Developer section stays, for changing it.

### Show device labels in the connections list

- Status: planned
- Consensus: no
- Source: Jonathan's note, 2026-11-11
- Problem: the connections list in the Developer section shows peer ids only.
  It does not say whose node each one is.
- Planned change: show each peer's device label next to its peer id.
- Open point: how a node learns another node's label. The label is sent to
  Sentry today, not to peers. Sending it to peers is a protocol addition
  (a field a node may send after the handshake) and makes the label public to
  every node it connects to.

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
- Found: 2026-11-11. About half of a node's lines are failed dials to
  addresses that can never be reached and "already connected or dialing"
  notices. Nodes pass on loopback and private addresses of other machines
  (`127.0.0.1`, `172.17.0.1`, `::1`) and stale records of old test nodes;
  each node then tries to dial them and logs a failure.
- Planned change:
  - Do not pass on or dial loopback and link-local addresses of other
    machines. This removes the cause, not only the lines.
  - Drop addresses of peers that have not been reachable for some time.
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

## 2.2.2: network health and mining view

### A block tracker and the average block time

- Status: planned
- Consensus: no
- Source: Jonathan's note, 2026-11-11
- Problem: the app shows only the current block height. It does not show
  whether blocks are arriving on time, so a stalled or racing chain is not
  visible. The stall on 2026-11-11 was found by reading the chain file.
- Planned change: a view of recent blocks (height, time, time since the block
  before it, difficulty, miner) and the average block time over the last
  blocks, next to the 300-second target, plus the time since the last block.

### Expected time to a block for this node

- Status: planned
- Consensus: no
- Source: Jonathan's note, 2026-11-11
- Problem: a miner cannot tell what to expect from their own GPU.
- Planned change: show the expected time for this node to find a block if it
  were the only one mining: current difficulty divided by this node's hash
  rate. State that it is an average and that a block can take much less or
  much more.

### The GPU use limit has no effect below 100%

- Status: planned
- Consensus: no
- Source: Jonathan's note, 2026-11-11
- Found: in the app, every setting below 100% gives about the same hash rate,
  and 100% is about ten times higher.
- Likely cause, from reading `gpu-miner/src/miner.rs`: the rest between GPU
  dispatches goes through `MinerControl::pause`, which sleeps in steps of
  100 ms. A dispatch takes about 30 ms, so any limit below 100% rests at
  least 100 ms for each 30 ms of work, about 20% to 25% use at every setting.
  `--bench` sleeps for the exact time, which is why the benchmark numbers in
  the 2.1.2 notes were proportional and the app is not. Not yet confirmed by
  a measurement in the app.
- Planned change: rest for the computed time, in steps no longer than what
  remains. Add a test of the rest loop's timing, and measure the hash rate
  through the mining loop, not only through `--bench`.

### Pass on relayed addresses, and reach nodes on the same network directly

- Status: planned; needs investigation first
- Consensus: no
- Source: Jonathan's note, 2026-11-11
- Problem: discovery depends on the relay's rendezvous list. Jonathan wants
  nodes to tell each other about nodes they reach through the relay as well,
  so a node can be found through any node that knows it.
- Related finding, 2026-11-11: two nodes on the same LAN (192.168.0.x)
  connected to each other through the relay. Neither tried the other's LAN
  address.
- To investigate: what the address exchange (`ADDR`, `GETADDR`) passes on
  today, whether relayed (`/p2p-circuit`) addresses are included, and why a
  LAN address was not tried.
- This interacts with "Stop sending lines that say nothing" in 2.2.1: pass on
  addresses that can be reached, not every address a node has.

## 2.2.3: wallet and block detail

### Saved addresses for sending

- Status: planned
- Consensus: no
- Source: Jonathan's note, 2026-11-11
- Problem: sending needs a 64-character address pasted in each time.
- Planned change: an address book in the app: save an address under a name,
  pick it when sending. Stored in the data folder.

### Full block view in the Developer section

- Status: planned
- Consensus: no
- Source: Jonathan's note, 2026-11-11
- Problem: the Developer section lists only blocks this node mined, with a
  few fields.
- Planned change: any block can be expanded to show all of it: header fields,
  the coinbase, each transfer. Click through to open more detail. This needs
  a node endpoint that returns a block by height or hash.

## 2.3: graphical overhaul

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

## Consensus backlog

Every open question about the consensus rules, in one place. A change here
makes old and new nodes disagree, so it ships only in a `2.x.0` release with
a higher block version. Work that is planned for a version has its entry
under that version; this list says where each question stands.

| Question | Stands |
| --- | --- |
| The difficulty rule compounds | Planned for 2.2.0 (entry under 2.2.0) |
| Hash power leaves and the chain stalls | To design, with 2.2.0 |
| A fallback against stalls of hours | To design, with 2.2.0 |
| Adjust every block or by epoch | To design, with 2.2.0 |
| Starting difficulty | Open, with 2.2.0 |
| Block version rises on every change | Planned for 2.2.0 |
| Merkle root for transfers | Deferred |

### Hash power leaves and the chain stalls

- Status: to design, with 2.2.0
- Consensus: **yes**
- Source: Jonathan's note, 2026-11-11
- Problem: friends mined with strong GPUs and then turned them off. The
  difficulty stayed at the level their GPUs had reached, and the remaining,
  weaker GPU could not find a block. Any rule that tracks hash power has this
  weakness to some degree: the difficulty can only come down as blocks are
  found, and blocks are slow exactly when it needs to come down.
- Note: on 2026-11-11 most of the stall came from the compounding bug, not
  from hash power leaving. Jonathan's estimate was that the difficulty meant
  about 1.6 hours per block with every GPU mining, so the rule was wrong even
  before anyone left. The corrected rule must still be tested for this case.
- To design: how fast the difficulty must fall when most hash power leaves,
  and what the cost is in the other direction (see the fallback entry).

### A fallback against stalls of hours

- Status: to design, with 2.2.0
- Consensus: **yes**
- Source: Jonathan's note, 2026-11-11
- Question: is there a safe way to fall back to a lower difficulty when no
  block has been found for some time, for example 20 minutes, so the chain
  cannot stall for hours?
- Known hazard: a rule that drops to the easiest difficulty after a fixed
  wait is easy to abuse. A miner sets a block's timestamp far enough ahead to
  claim the easy difficulty, and can then mine many easy blocks. Bitcoin's
  test network has this rule and has block storms because of it. "Safe" here
  means the easing is small for each block and cannot be claimed by a
  timestamp alone.
- Candidates to evaluate, none decided:
  - Let one slow block count for more in the average (a larger solve-time
    cap than 4 x 300 s), so the difficulty falls faster after a stall. This
    stays inside the LWMA and has no new attack.
  - Ease the target by a bounded step for each period the block's own
    timestamp is past its parent's. The timestamp limits (after the median of
    the last 11 blocks, at most 2 hours ahead of the clock) bound what a
    miner can claim.
- Each candidate goes through the chain simulation tests planned under 2.2.0
  before one is chosen.

### Adjust every block or by epoch

- Status: to design, with 2.2.0
- Consensus: **yes**
- Source: Jonathan's note, 2026-11-11
- Question: adjusting from a single block lets one lucky block move the
  difficulty. Adjusting by a long epoch means a drop in hash power is fought
  for a whole epoch. Is there something better?
- Answer so far: the two are not the only choices. A correct LWMA adjusts on
  every block, from the average over the last N blocks, with recent blocks
  weighted more. One lucky block then moves the difficulty by about 2/N of
  the gap, and a real change in hash power is followed within a part of the
  window. What the chain ran in 2.0.0 to 2.1.2 was neither: it applied a
  window average to the parent's difficulty on every block, which compounds.
- Open point: the window size N. A smaller N follows changes faster and is
  moved more by luck. 60 was the value in use.

### Starting difficulty

- Status: open, with 2.2.0
- Consensus: **yes** (it is part of the genesis)
- Problem: the floor has been wrong in both directions. 35 bits was hours of
  work for the weakest node; 28 bits was 2 seconds for a laptop GPU.
- With a rule that adjusts from the first block, the starting value matters
  much less. Choose it for the hash power expected at the restart.

### Block version rises on every consensus change

- Status: planned for 2.2.0
- Consensus: **yes**
- The block version is the signal other clients read (see "Tell the user when
  the network has moved on"). It rises with every consensus change, also when
  the chain restarts, and is never reused. The restarted chain is version 3.
  `consensus_version` in `jcoin/src/consensus.rs` already selects the version
  by height.

### Merkle root for transfers

- Status: deferred
- Consensus: **yes**
- A block commits to its transfers with one hash over all of them. A Merkle
  root is needed only for a light client that checks one transfer without the
  whole block. No such client exists. See `docs/adr/0001-restart-consensus-v2.md`.

## Unscheduled

Problems and wanted changes that have no version yet.

### Pending transfers are lost when the node restarts

- Consensus: no
- Found: 2026-11-11. Pending transfers are kept in memory only. When the
  sender's app was closed with two transfers pending, its node forgot them.
  They can still confirm if another node holds them; otherwise they must be
  sent again. The app's "Sent this session" list is also lost on restart, so
  the sender cannot see which transfers are affected.
- Possible change: store pending transfers in the data folder and reload them
  at startup; keep the sent list across restarts.

### Two copies of the app can run on the same data folder

- Consensus: no
- Nothing stops a second copy of the app from starting while one is running.
  Both then write to the same chain file and key folder. Since 2.1.2 a
  downloaded copy and the installed copy make this easier to do by accident.
- Possible change: a lock in the data folder; a second copy brings the first
  one's window forward and exits.

### The miner puts at most one transfer in a block

- Consensus: no (the rules already allow 64)
- The node's own miner and the mining endpoints select one pending transfer
  for each block. Planned change: select up to 64, highest fee first, each
  sender's transfers in nonce order.

### Hole punching from Windows

- Consensus: no
- Since 2.1.1, a Windows node dials from a new port, because dialing from the
  listen port fails there (os error 10048). The library's hole-punch dials
  still use the listen port. A friend's Windows node logged four failed hole
  punches and one that succeeded, so it works some of the time. A fix would
  be in how the Windows sockets are set up.

### The app does not show a transfer that is on its way in

- Consensus: no
- The app shows only the confirmed balance. A recipient sees nothing until the
  transfer is in a block.

## Operations (no release needed)

- Create a Sentry user auth token (scopes `org:read`, `project:read`,
  `event:read`) and save it to `~/.config/jeffycoin/sentry-token`, so
  `tools/sentry-logs` can read the logs.
- Add the `SENTRY_AUTH_TOKEN` repository secret, so that crash reports show
  function names and line numbers.
- Set up Sentry alerts: one for any crash, one for `reservation_lost` with no
  `reservation_ok` after it. Nothing notifies anyone today.
- Check the Sentry plan's log quota against the volume once several nodes run
  all day.
- Deploy a current relay program to the Lightsail relay, so that refused
  reservations appear in its log.
- Deploy a current node to the iMac reference node. It still runs a build with
  the earlier 35-bit genesis and cannot sync with 2.x nodes. After 2.2.0 it
  needs that version.
- The relay and the iMac node do not report to Sentry; they log locally only.
  Decide whether the command-line node and the relay should report too.
