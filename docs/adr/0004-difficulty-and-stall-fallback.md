# ADR 0004 — Difficulty rule, stall fallback, and block version

- Status: Accepted (2026-11-11), shipped in 2.2.0
- Related: `0001-restart-consensus-v2.md`

## Context

The chain of 2.0.0 to 2.1.2 stalled at height 69. Its difficulty rule
applied a window average to the parent's difficulty on every block, so the
adjustment compounded; by Jonathan's estimate the difficulty meant about 1.6
hours per block with every GPU mining. Friends then turned their GPUs off,
and the remaining, weaker GPU could not find a block.

Any rule that tracks hash power has this weakness to some degree: the
difficulty can only come down as blocks are found, and blocks are slow
exactly when it needs to come down.

## Decisions

**Adjust every block, LWMA over a window of 30.** Adjusting from one block
lets a lucky block move the difficulty; adjusting by epoch fights a drop in
hash power for a whole epoch. An LWMA adjusts on every block from the
weighted average of the last N solve times. One lucky block moves the
difficulty by about 2/N of the gap. Window 30 recovers faster than 60; the
difficulty wanders about 0.28 bits with constant hash power, against 0.18.
One solve time counts for at most 30 minutes.

**Stall fallback.** The target doubles 20 minutes after the parent's
timestamp and again every 10 minutes (`consensus::eased_target`,
`Params::stall_grace`, `Params::stall_step`).

**Timestamps at most 10 minutes ahead** of the receiving node's clock
(`block::MAX_FUTURE_DRIFT`). With the limit below the 20-minute grace
period, a timestamp alone claims no easing. With a 2-hour limit, a miner
could stamp a block ahead and claim one block about 2,000 times easier. An
eased block also carries less work, so a normal-difficulty block at the
same height wins. Bitcoin's test network has a fixed-wait fallback without
such a bound and has block storms because of it.

**Starting difficulty 28 bits** (part of the genesis). The rule adjusts from
the first block, so the start matters little. It is also the easiest
difficulty the fallback can reach, so it is set for the weakest node mining
alone: about 300 s for a CPU at 1 MH/s.

**The block version rises with every consensus change**, also when the
chain restarts, and is never reused. The restarted chain is version 3.
`consensus_version` in `jcoin/src/consensus.rs` selects the version by
height. Other clients read it to show the newer-rules banner.

## Evidence

`cd jcoin && cargo test --release stall_report -- --ignored --nocapture`.
Medians over 25 seeds; hash power falls to 1/15 or 1/100:

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
fallback bounds the wait for the next block at about an hour whatever the
drop. With constant hash power it changes little: mean block time 295 s.

## Consequences

- A node whose clock is more than 10 minutes slow refuses current blocks.
- A miner with a large share of hash power can still shade timestamps to
  lower the difficulty somewhat (`docs/KNOWN-ISSUES.md` V3).
