# JeffyCoin — known issues and vulnerabilities

These are design/implementation gaps in the code beside this file,
not bugs in the normal path (a full transaction cycle across gossiping nodes
works and is tested). Items are grouped by whether they are security-relevant
(could be exploited or cause loss) or operational. Numbers are stable
identifiers; resolved items are removed, not renumbered.

## ⚠️ Security-relevant vulnerabilities

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

### V4. Reorgs up to 288 blocks; no checkpoints
A node follows a heavier valid chain that forks at most `MAX_REORG_DEPTH`
(288) blocks below its tip, so a partition or a transient majority can revert
up to about a day of "confirmed" blocks (double-spend risk on reorg). A deeper
fork is refused (ADR 0005), so after a partition longer than a day two nodes
can stay on different chains until one is restarted from the other's chain.
There are no checkpoints and no confirmation-depth guidance shown to users.

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
dial each other at the same moment. Harmless (gossip dedups). Extra direct
connections are closed by the smaller peer id; if the closed one carried the
session, the session is reopened on the kept one.

### O9. The app's test program sometimes crashes when it exits
All app tests pass, then the test program ends with SIGSEGV: about 1 run
in 20 for `tools/check app`, and every run of the GPU end-to-end test
(`tools/check --gpu`). It happens on `main` at 2.2.2 as well
(`tools/repeat-test --at main app tests 20`). Likely cause, not confirmed:
threads the tests leave running (node, miner, GPU device) are torn down
while the process exits. Fix direction: stop the backend's threads and drop
the GPU before a test returns. It affects tests only, not the app.

## Suggested fix order
1. **V2** basic peer scoring + disconnect/ban on repeated invalid data.
2. **V3** analyse how far timestamp shading lowers the difficulty under the
   LWMA and the stall fallback.
3. **V4** optionally show a confirmation-depth recommendation; consider
   checkpoints if the network is ever adversarial.
