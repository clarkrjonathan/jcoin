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

2.2.2 is released. The next planned release is **2.2.3**.

## After 2.2.0 (released 2026-11-11)

2.2.0 restarted the chain; see `CHANGELOG.md`. Follow-up that needs no code:

- Tell everyone on 2.1.2 or earlier to update; those versions cannot show the
  newer-rules banner.
- Confirm on a Windows PC: the app starts on the new chain, mines, and the
  install and update flows work. Nothing in 2.2.0 ran on Windows before the
  release.
- Watch the live chain's block times against the simulations (window 30,
  fallback at 20 minutes).
- Redeploy the relay and the iMac node (see "Operations").

## Left over from 2.2.3

### Several direct connections between two nodes

- Status: planned
- Consensus: no
- Source: seen while building 2.2.3, with `tools/local-net up 2 relay`
- Problem: both nodes dial each other's direct address, and a hole punch can
  add one more, so a pair can keep up to three direct connections. Only one
  carries the session; the others stay open.
- Planned change: only the smaller peer id dials (as for sessions), or close
  direct connections that carry no session once one does. The host cannot
  yet tell which connection carries the session.

### Address book entries never expire

- Status: not planned yet; decide after 2.2.3's direct connections have been
  seen in the field
- Consensus: no
- Source: investigation for 2.2.3, 2026-11-11
- Problem: relayed addresses are already passed on (a node's own circuit
  address goes out in every `ADDR`, plus up to 64 book entries), so a node
  can be found through any node that knows it. But entries never expire,
  and the 64 are taken in hash-set order, not the most recent.
- Planned change: expire entries not seen for a while, and pass on the most
  recently seen first.

## 2.2.4: wallet and block detail

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
| The difficulty rule compounds | Fixed in 2.2.0 |
| Hash power leaves and the chain stalls | Addressed in 2.2.0 by the fallback |
| A fallback against stalls of hours | Decided and built in 2.2.0 |
| Adjust every block or by epoch | Decided in 2.2.0: every block, window of 30 |
| Starting difficulty | Decided in 2.2.0: 28 bits |
| Block version rises on every change | Built in 2.2.0 (version 3) |
| Merkle root for transfers | Deferred |

### Hash power leaves and the chain stalls

- Status: addressed in 2.2.0 by the stall fallback; watch on the live chain
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

- Status: decided 2026-11-11 and built in 2.2.0
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
- Simulated 2026-11-11 (`cargo test --release stall_report -- --ignored
  --nocapture`; medians over 25 seeds; hash power falls to 1/15 or 1/100):

  | Window | Fallback | Drop | First block | 10 blocks | Back under 2x target |
  | --- | --- | --- | --- | --- | --- |
  | 60 | none | 15x | 0.8 h | 7.8 h | 16 h |
  | 60 | none | 100x | 5.1 h | 49 h | 87 h |
  | 30 | none | 15x | 0.7 h | 6.3 h | 8 h |
  | 60 | x2 at 20 min, then each 10 min | 15x | 0.5 h | 3.9 h | 5.8 h |
  | 60 | x2 at 20 min, then each 10 min | 100x | 1.0 h | 6.2 h | 8.9 h |
  | 30 | x2 at 20 min, then each 10 min | 15x | 0.6 h | 3.3 h | 3.1 h |
  | 30 | x2 at 20 min, then each 10 min | 100x | 1.0 h | 4.8 h | 4.9 h |

  A larger solve-time cap alone helps little (16 h to 11.5 h at cap 12). The
  timestamp fallback bounds the wait for the next block at about an hour
  whatever the drop. With constant hash power it changes little: mean block
  time 295 s, difficulty spread 0.22 to 0.28 bits against 0.18.
- Abuse of the timestamp fallback: a miner can stamp a block up to the
  future-drift limit ahead and claim easing for time that has not passed.
  With the limit at 2 hours that is one block about 2,000 times easier. With
  the limit below the 20-minute grace period (for example 10 minutes) a
  timestamp alone claims nothing. An eased block also carries less work, so
  a block at normal difficulty at the same height wins.
- Decided 2026-11-11 (Jonathan): window 30; the target doubles 20 minutes
  after the parent and again every 10 minutes; nodes accept a timestamp at
  most 10 minutes ahead of their clock. Built as `consensus::eased_target`,
  `Params::stall_grace` / `stall_step`, and `block::MAX_FUTURE_DRIFT`.
- Known cost: a node whose clock is more than 10 minutes slow refuses current
  blocks.

### Adjust every block or by epoch

- Status: decided in 2.2.0: every block, from a window of 30
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
- Window size: 30, chosen from the stall simulations (faster recovery; the
  difficulty wanders about 0.28 bits with constant hash power, against 0.18
  at 60).

### Starting difficulty

- Status: decided in 2.2.0: 28 leading zero bits
- Consensus: **yes** (it is part of the genesis)
- Problem: the floor has been wrong in both directions. 35 bits was hours of
  work for the weakest node; 28 bits was 2 seconds for a laptop GPU.
- With a rule that adjusts from the first block, the starting value matters
  much less. It is also the easiest difficulty the stall fallback can reach,
  so it is set for the weakest node mining alone: 28 bits is about 300 s for
  a CPU at 1 MH/s.

### Block version rises on every consensus change

- Status: built in 2.2.0
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

### A notice for a heavier chain without a higher block version

- Consensus: no
- Source: Jonathan's note, 2026-11-11
- 2.2.0 counts refused blocks that carry a **higher version**. A refused
  chain with much more work and the same version gets no notice yet. The
  difficulty: a block whose parent is unknown is also what a node sees
  during ordinary sync, so it cannot be counted as evidence by itself.
- Possible change: count such blocks only when they stay unconnected after a
  sync attempt, and show a weaker notice.

### Peers' rule version in the connections list

- Consensus: no
- Show which peers run other rules. Needs the version in the handshake (a
  protocol addition).

### Pending transfers are lost when the node restarts

- Consensus: no
- Found: 2026-11-11. Pending transfers are kept in memory only. When the
  sender's app was closed with two transfers pending, its node forgot them.
  They can still confirm if another node holds them; otherwise they must be
  sent again. The app's "Sent this session" list is also lost on restart, so
  the sender cannot see which transfers are affected.
- Possible change: store pending transfers in the data folder and reload them
  at startup; keep the sent list across restarts.

### The updater checks the checksum but not a signature

- Consensus: no
- Source: Jonathan's question, 2026-11-11
- Today `update::install` fetches `SHA256SUMS-<platform>.txt` from the release,
  hashes the downloaded binary, and refuses to install on a mismatch
  (`app/src/update.rs`). This catches a corrupt or partial download, but it is
  integrity, not authenticity: the checksum list is fetched over HTTPS from the
  same GitHub release and is not signed, so anyone who could publish to that
  release (or tamper with both files) could supply a matching checksum for a
  swapped binary.
- Possible change: sign `SHA256SUMS` in the release workflow with a key whose
  public half is baked into the app, and verify the signature before trusting
  the list. Low priority for the current friends-only use, but it is the gap.

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
- The `readme-releases` workflow fills the "Latest releases" table in
  `README.md` from the GitHub Releases on a release change or manual dispatch,
  and pushes the result to `main`. For its push to land, `main`'s branch
  protection must let `github-actions[bot]` commit (or run it by hand). Run it
  once (Actions → readme-releases → Run workflow) to populate the table.
