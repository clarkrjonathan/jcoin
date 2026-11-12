# ADR 0005 — 2.2.4: transfers survive restarts and reorgs; bounded memory

- Status: Accepted (2026-11-11, Jonathan)
- Related: `docs/KNOWN-ISSUES.md` V1, V4, O1, O5, O8; `planned.toml`
  release `reliability`

## Context

Friends can lose a transfer without noticing: it is forgotten on restart,
dropped on a reorg, or rejected when it arrives before the transfer it
follows. A node's memory can grow without limit from fork blocks. These come
before the wallet features (saved addresses, block view), which move to the
next patch. Peer scoring (V2), the miner filling blocks with 64 transfers,
and address book expiry stay in the backlog.

None of the changes alter which blocks are valid, so 2.2.4 is a patch.

## Decisions

**Fork storage: keep side branches within 288 blocks of the tip** (about a
day at 5-minute blocks). Side branches that fork deeper are dropped. A node
therefore refuses a heavier chain that forked more than 288 blocks back:
this is a maximum reorg depth (V4). A refused reorg is logged. Chosen over
keeping headers past the limit and fetching bodies again, which is more work;
with a few friends' GPUs a partition of a day is unlikely.

This is node policy, not a block validity rule. Old and new nodes agree on
which blocks are valid, but after a partition longer than a day a 2.2.4 node
can stay on a lighter chain that an older node leaves.

**Pending transfers** are stored in the data folder and reloaded at startup.
A stored transfer that is no longer valid (confirmed, or no longer
affordable) is dropped without notice. The app's sent list is kept across
restarts.

**Reorg re-queue**: transfers from the dropped branch go back through normal
mempool admission; invalid ones fall out there.

**Out-of-order transfers**: each sender may have up to 16 held, each for up
to 10 minutes. They are admitted when the nonce gap fills.

**Direct connections**: of a peer's direct connections, the one the smaller
peer id dialed is kept (the oldest, if several); without one, the oldest.
Only the smaller peer id closes the others, so the two sides never close
different ones. The host cannot tell which connection carries the session
(libp2p-stream gives no connection id); if the closed one did, the session
ends and the host opens it again on the kept connection.
