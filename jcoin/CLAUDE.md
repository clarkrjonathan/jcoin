# jcoin package — working notes

The node, wallet, relay, and all consensus code. `cargo test --release` here,
or `tools/check jcoin` from the repository root.

## Files

| File | Holds |
| --- | --- |
| `src/lib.rs` | `Transaction` (153 bytes, Ed25519), ids, signing bytes |
| `src/consensus.rs` | `Params` (`MAINNET`, `REGTEST`), `LATEST_VERSION`, `consensus_version`, `BlockContext` (`target_at`), `eased_target` (stall fallback) |
| `src/block.rs` | Header (156 bytes), `Block`, `validate_and_apply` (the whole deterministic rule set), `assemble_block`, `build_and_mine`, `mine_chunk`, `MAX_FUTURE_DRIFT`, `MAX_TXS` |
| `src/chain.rs` | Block store, fork choice by cumulative work, `MAX_REORG_DEPTH` and `prune` (bounded forks), `lwma_next_target` (difficulty rule), `context_for_child`, `work_since`; the chain simulations are in its tests |
| `src/state.rs` | Account balances and nonces, `state_root` |
| `src/mempool.rs` | Pending transfers by sender and nonce, the out-of-order hold, `pending`, `best_applicable`, `prune` |
| `src/node.rs` | Node core: `NodeRequest`, `run_node` (owns chain, mempool, signals; replays `blocks.dat`, keeps `pending.dat`; re-queues transfers after a reorg) |
| `src/signal.rs` | Counts refused blocks with a newer version and real work (`Signals`, `Level`) |
| `src/service.rs` | The node as an embeddable service: `ServiceConfig`, `run`, REST handlers, built-in CPU miner |
| `src/wallet.rs` | Key files, `sign_transfer` |
| `src/store.rs` | Append-only block log, torn-tail repair |
| `src/log.rs` | `log_info!`, `log_error!`, optional sink (the app sets one) |
| `src/amount.rs` | `COIN`, decimal JEF parse and format |
| `src/p2p/` | Peer-to-peer; protocol in `src/p2p/README.md` |
| `src/bin/` | `node`, `wallet`, `relay` (thin front ends), `attack`, `hashbench` |

`src/p2p/`: `host.rs` (libp2p swarm, relay reservation and retry, dialing),
`relay_host.rs` (the relay), `peer.rs` (handshake, session loop, sync,
pending-transfer announcements), `network.rs` (sessions, address book,
connection records), `message.rs` and `frame.rs` (wire format).

REST routes (`service.rs`): `POST /wallet/transaction`,
`GET /wallet/transaction/{id}`, `GET /balance/{address}`, `GET /chain`,
`GET /chain/recent?count=N`, `GET /peers`, `GET /network/signal`, `GET /mining/work/{miner}`,
`POST /mining/submit`.

## Consensus facts (version 3, since 2.2.0)

- Account model. A block is a coinbase plus up to 64 transfers; the miners
  pack at most one.
- Proof of work: `SHA256(header) < target`. Fork choice: most cumulative work.
- Difficulty: LWMA over 30 blocks, one solve time capped at 6 x 300 s,
  adjusting from block 1. Stall fallback: the target doubles when a block's
  timestamp is 20 minutes after its parent's, then every 10 minutes, down to
  `pow_limit` (28 bits).
- Timestamps: above the median of the last 11 blocks (consensus); at most 10
  minutes ahead of the clock (local rule, in `node.rs`, never in
  `validate_and_apply`).
- Every consensus input reaches validation and mining through `BlockContext`.
  Do not derive one anywhere else.
- `REGTEST` has no difficulty adjustment and no fallback, so tests mine at
  `pow_limit` with any timestamps.

## Tests

- Unit tests are in each file. Rule tests that need adjustment use
  `adjusting(window)` or `mainnet_rule()` in `chain.rs`, not `REGTEST`.
- `simulate` in `chain.rs` runs a chain against a hash-rate function with
  random solve times. Any change to the difficulty rule must keep
  `a_miner_far_faster_than_the_floor_settles_without_running_away`,
  `hash_rate_steps_up_and_down_are_followed`, and
  `the_chain_keeps_moving_when_hash_power_leaves` passing, and should be
  compared with `stall_report` (ignored; prints a table).
- Tests that need a running core spawn `run_node` on a channel; see the
  helpers at the top of the test module in `node.rs`.
