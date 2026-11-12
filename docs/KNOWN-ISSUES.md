# JeffyCoin — known issues and vulnerabilities

Current as of 2.2.3. These are design/implementation gaps in the current code,
not bugs in the normal path (a full transaction cycle across gossiping nodes
works and is tested). Items are grouped by whether they are security-relevant
(could be exploited or cause loss) or operational. Numbers are stable
identifiers; resolved items are removed, not renumbered.

## ⚠️ Security-relevant vulnerabilities

### V1. Unbounded fork storage (memory exhaustion / DoS)
`chain.rs` never prunes: every valid block it ever accepts is kept forever, and
each stored block holds a full `State` clone. A peer can send an endless stream
of valid-but-losing fork blocks (or sustained natural forking under bad delay)
and grow a node's memory without limit. No cap, no pruning, no fork-retention
policy. **Highest-priority fix.**

### V2. No misbehaviour scoring or peer banning (DoS)
A peer that floods invalid transactions, invalid blocks, or garbage frames is
never penalized or disconnected for it. Decoding errors are skipped and the
connection stays open. There is no blacklist (the vault notes call for one). A
single malicious peer can keep a node busy.

### V3. Time-warp / difficulty manipulation
Difficulty comes from block timestamps (an LWMA over the last 30 blocks, plus a stall fallback that eases a block whose timestamp is 20 minutes
after its parent's). Guards: median-time-past bounds timestamps from below,
one solve time counts for at most 30 minutes, and a node accepts a timestamp
at most 10 minutes ahead of its clock, which is shorter than the fallback's
20 minutes. A miner with a large share of hash power can still shade
timestamps to lower difficulty somewhat. Not analysed beyond the simulations
in `jcoin/src/chain.rs`.

### V4. No finality, no max-reorg-depth, no checkpoints
A node will reorg to any heavier valid chain at any depth, so a long network
partition or a transient majority can revert many already-"confirmed" blocks
(double-spend risk on reorg). This is inherent to Nakamoto consensus, but there
is no safety rail (no checkpoint, no maximum reorg depth, no confirmation-depth
guidance surfaced to clients).

### V5. Local-only future-time bound
The 10-minute future-timestamp bound is checked against the receiving node's
wall clock only (not network-adjusted time). A node with a badly wrong clock can
mis-accept or mis-reject blocks until its clock corrects. Non-consensus by
design, but worth noting.

### V6. Relay trust
Every connection, relayed ones included, is encrypted end to end with Noise
and authenticated by libp2p peer id, so neither the relay nor a man in the
middle can read or alter traffic or impersonate a node. What remains: the relay
can refuse service, drop circuits, or omit peers from rendezvous answers, and a
node behind NAT that knows no other peer depends on it. Peer ids are not tied
to any allowlist, so anyone can join an open network. Fix directions: support
several relays per node, persist the address book, and add peer scoring (V2).

## Operational limitations (correctness-safe, but rough edges)

### O1. Reorg drops mempool transactions
On a reorg the mempool is only pruned to the new tip. Transactions that were
confirmed on the orphaned branch are not re-queued, so they disappear until
the sender resubmits. The chain stays consistent. A resubmitted transaction is
admitted and relayed.

### O2. Out-of-order blocks are re-synced, not buffered
A block whose parent is missing is dropped and triggers a full block-locator
sync, rather than being held until its parent arrives. Correct, but chatty under
heavy delay, and it depends on the delivering peer staying connected.

### O4. Persistence is append-only and uncompacted
The block log (`store.rs`) only grows and is never compacted; state is not
snapshotted (it is replayed from all blocks on startup), so startup cost grows
with chain length.

### O5. Duplicate peer links
The host opens at most one JeffyCoin session per peer (by libp2p peer id) and
keeps it across a hole-punch upgrade. Two sessions can appear when two nodes
dial each other at the same moment. Harmless (gossip dedups). A pair can also
hold several direct connections (`tools/planned show direct-dupes`).

### O8. Queued transactions that arrive out of order are rejected
A sender may queue transactions with consecutive nonces, and the mempool
accepts nonce `n + 1` only after `n`. Gossip over one link is in order, but a
node that receives `n + 1` from one peer before `n` from another rejects
`n + 1` as a nonce gap and does not keep it. The transaction still reaches
that node's miners only if another peer relays it again later, which does not
happen today. Fix direction: hold out-of-order transactions in a small
per-sender buffer and admit them when the gap fills.

### O9. The app's test program sometimes crashes when it exits
All app tests pass, then the test program ends with SIGSEGV: about 1 run
in 20 for `tools/check app`, and every run of the GPU end-to-end test
(`tools/check --gpu`). It happens on `main` at 2.2.2 as well
(`tools/repeat-test --at main app tests 20`). Likely cause, not confirmed:
threads the tests leave running (node, miner, GPU device) are torn down
while the process exits. Fix direction: stop the backend's threads and drop
the GPU before a test returns. It affects tests only, not the app.

## Suggested fix order
1. **V1** bound/prune the fork store (cap by work-distance from tip or by count).
2. **O1** re-queue transactions from orphaned blocks on reorg.
3. **V2** basic peer scoring + disconnect/ban on repeated invalid data.
4. **V3** analyse how far timestamp shading lowers the difficulty under the
   LWMA and the stall fallback.
5. **V4** optionally expose a confirmation-depth recommendation; consider
   checkpoints if the network is ever adversarial.
