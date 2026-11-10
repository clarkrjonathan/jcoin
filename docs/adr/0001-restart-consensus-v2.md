# ADR 0001 — Restart on a consensus v2 genesis

- Status: Proposed (2026-11-10)
- Supersedes: the current `initial-vertical` demo chain (test vehicle, kept only
  for continuity testing; see `../HANDOFF.md`).

## Context

The persisted demo chain is a development vehicle, not a chain of value. It can
be scrapped. A set of changes has accumulated that are consensus-level: each one
changes how a block is validated, so a node running the new rule rejects a block
from a node running the old rule. The chain has no consensus-version signaling
today (see `../KNOWN-ISSUES.md`), so there is no way to activate a new rule on
the running chain. Any one of these changes already forces a fresh genesis.

Rather than restart once per change, cut one new genesis that carries all of
them, plus the versioning machinery that lets the *next* change happen without a
restart.

Observed problems that motivate the batch:

- **Retarget is slow to absorb hashrate steps.** The epoch retarget (window 12,
  4x clamp) takes two to three epochs to react to a large change in mining
  power. Real steps on the demo chain (the iMac at height 152, release builds at
  180, the GPU at 216) each drove block time far below the 300 s target for
  24 to 36 blocks. The formula is unbiased; it is just slow. More miners does not
  fix this, because the swings come from changes in *total* hashrate, not from
  per-block luck.
- **One transfer per block.** A v1 body is a coinbase plus zero or one normal
  transaction (`block.rs` `split_body`, the `rest.len() > 1` guard). Throughput
  is about 12 transactions per hour. Not a live bottleneck yet, but capacity is a
  consensus rule, so it cannot be raised later without another restart.
- **`pow_limit` is miscalibrated.** It is `limit_with_zero_bits(20)`: about a
  million expected hashes per block, which a weak GPU clears in under 0.01 s. A
  new chain spends roughly 150 blocks ramping difficulty up from that floor
  before block time approaches the target.
- **Genesis timestamp artifact.** When the genesis timestamp is far from real
  time, the first retarget epoch's span clamps and hardening starts one epoch
  late (noted in `../HANDOFF.md`).

## Decision

Cut a new genesis (`MAINNET` v2) with the following rule set.

### 1. Per-block LWMA retarget

Replace the epoch retarget with a per-block Linearly Weighted Moving Average
(LWMA) over a trailing window of N blocks. LWMA reacts within a few blocks
instead of a few epochs.

Keep the timestamp guards that make a per-block retarget safe against time-warp
(a short window gives a manipulated timestamp more leverage over the next
difficulty):

- Median-time-past of the last 11 blocks as the consensus lower bound on a
  timestamp. Already present in `chain.accept`.
- The 2-hour forward drift cap as a local admission rule. Already present.
- Clamp each block's solvetime inside the LWMA sum (floor at a small positive
  value, cap at a multiple of the target). Part of standard LWMA.
- Keep a per-step change clamp as a backstop.

This is a code change in the retarget function, not just a parameter change. Add
the LWMA window N as a `Params` field.

Open parameter: **N** (LWMA window). Typical values are 60 to 90 blocks.

### 2. Multiple transactions per block

Relax the body rule from "zero or one normal transaction" to "up to `MAX_TXS`
normal transactions." The validation engine already handles many: `block.rs`
`validate_and_apply` loops over the normals, sums fees across all of them,
verifies every signature, and applies each to the running state in body order.
`transactions_hash` already hashes the whole set. The only gate is the
`rest.len() > 1` check in `split_body`; change it to `rest.len() > MAX_TXS`.

Miner work (not consensus): select up to `MAX_TXS` transactions from the mempool,
highest fee first, and order each sender's transactions by nonce (state
application checks the nonce as it goes, so an out-of-order block is rejected).
The queued pending-transaction work already sequences nonces per sender.

Keep the flat `transactions_hash`. A Merkle root is only needed for light-client
inclusion proofs (SPV), which this network does not use; it is a clean drop-in
later behind the same header field.

No byte or weight cap for now; a transaction count cap is simpler and enough at
this scale.

Open parameter: **`MAX_TXS`** (suggest 64).

### 3. Reevaluate `pow_limit`

Set the genesis target (which equals `pow_limit`; `block.rs` `genesis`) so the
first blocks land near the 300 s target for the expected starting hashrate,
instead of starting at a trivial floor.

Measured, release build (2026-11-10):

| Miner | Rate |
| --- | --- |
| Laptop CPU | 2.30 MH/s per core (3 cores = 6.9 MH/s) |
| Laptop GPU (Intel Iris Xe) | ~99 MH/s (from `demo/gpu-miner.log`) |
| iMac CPU (i5-4570R, one thread) | 0.98 MH/s |

The current launch field is the laptop plus the iMac: the GPU dominates at
~100 MH/s total. At 300 s per block that is ~3.0e10 hashes per block, which is
**~35 leading-zero bits** — against the current `pow_limit` of 20 bits (~1e6
hashes per block), about 30,000x too easy, which is why a fresh chain ramped for
~150 blocks.

Set `pow_limit` to 35 leading-zero bits (err slightly easy: a too-hard genesis
makes the first block look broken; LWMA hardens a slightly-easy start in a few
blocks):

```rust
// ~100 MH/s launch field, 300 s/block, ~34.8 bits
pub const POW_LIMIT: [u8; 32] = [
    0x00, 0x00, 0x00, 0x00, 0x24, 0x8a, 0x6f, 0x98, 0x2f, 0xe5, 0x28, 0xe0, 0xa5, 0x98, 0x78, 0x8c,
    0x8d, 0x4c, 0xac, 0x6b, 0x4a, 0xb8, 0x5f, 0x6d, 0x79, 0xb3, 0x38, 0x36, 0x5f, 0x8b, 0x79, 0xaf,
];
```

Implementation note: the code uses `limit_with_zero_bits(35)` (exactly 35 leading
zero bits, byte 4 = `0x1f`) rather than the hand-written array above, which was
the raw `hashbench` output for ~34.8 bits. The clean constant matches this ADR's
pinned "35 bits" decision; the sub-bit difference is absorbed by LWMA.

The desktop GPU and friends' GPUs are not in the launch field and were not
reachable to benchmark from here. They do not change `pow_limit`: the LWMA
retarget (item 1) absorbs them when they join. Each doubling of total hashrate
adds one zero bit, so even a 1 GH/s desktop GPU at launch would move this to
~38 bits, a one-line change if the launch set grows.

Note: a future FPGA or ASIC SHA-256 miner would be a large hashrate step. The
LWMA retarget in item 1 is what absorbs it; `pow_limit` only sets the start.

### 4. Real genesis timestamp and updated marker

Set `genesis_timestamp` to the actual launch time (or close to it), so the first
retarget window spans real time and hardening begins on schedule.

Update the genesis marker to `"I am 21 years old, I love Jeffy!"`. The marker is
not a separate field: `genesis()` stores it in the genesis block's
`previous_block_hash` slot (`block.rs`), which has no real parent to point at, so
it must be exactly 32 bytes. The new string is 32 bytes, like the old one.

### 5. Height-activated consensus versioning

Add the machinery that lets a future rule change activate without a new genesis.

- Keep the `version: u32` header field.
- Add `consensus_version(height) -> u32` to `Params`, in the same style as the
  existing `block_reward(height)`.
- In `validate_and_apply`, require `header.version == consensus_version(height)`
  instead of the current equality against a single `BLOCK_VERSION` constant.
- A future consensus change: implement the new rules behind a version number,
  pick an activation height in the future, and ship it to every node before that
  height. Nodes validate by the version the height selects. No restart.

The v2 genesis launches at version 2. Version 1 is the current demo chain's rule
set and is not carried forward.

## Non-goals

- **ASIC resistance.** Keep SHA-256. The intent is a GPU-friendly (and
  FPGA/ASIC-friendly) proof of work. A custom FPGA or ASIC miner is a welcome
  future project, not a threat to design against.
- **Merkle transaction tree.** Deferred until there is a light client that needs
  inclusion proofs.
- **Byte or weight based block limit.** Deferred; a count cap is enough now.
- **Compact difficulty encoding.** The header stores a full 32-byte target.
  Shrinking it to a 4-byte compact form is a possible later size optimization,
  not part of this restart.

## Open parameters to pin before cutting genesis

| Parameter | Where | How to decide |
| --- | --- | --- |
| `pow_limit` | `consensus::MAINNET` | **pinned: 35 bits** (measured ~100 MH/s launch field) |
| LWMA window `N` | new `Params` field | pick in the 60 to 90 range |
| `MAX_TXS` | body rule in `split_body` | suggest 64 |
| `genesis_timestamp` | `consensus::MAINNET` | the launch time |
| genesis marker | `block.rs` `GENESIS_MARKER` | **pinned: "I am 21 years old, I love Jeffy!"** (must be exactly 32 bytes) |

## Migration

- The new genesis is incompatible with the v1 demo chain by design (different
  genesis, different rules). v1 and v2 nodes reject each other's blocks.
- Archive the v1 demo data directories before wiping, in case a comparison is
  useful. The chain holds no value.
- Rebuild all miners from the v2 code and start them on the new genesis.
- Update `../HANDOFF.md`, `../p2p-protocol.md`, and `../KNOWN-ISSUES.md` to the
  v2 rules in the same change.
