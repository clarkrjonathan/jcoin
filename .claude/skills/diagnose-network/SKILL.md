---
name: diagnose-network
description: Find out why JeffyCoin nodes do not connect, do not sync, hold a pending transfer, or why blocks are slow or stalled. Use for questions like "did they connect", "why is the chain stuck", "when will my transfer land", "is the iMac on the right version". Gives the order of cheap checks and the causes seen before.
---

# Diagnosing the network

Work from the cheapest evidence to the most expensive. Stop when the
question is answered.

## 1. This computer's node (one command)

`tools/node-status` gives height, tip difficulty, connections (how many
exchange blocks, how many through the relay), the newer-rules signal, and the
app key's balance and pending amount. For another local node, give its REST
port.

## 2. The chain itself

`tools/chain-dump --tail 20` shows recent block times and difficulty, the
average block time, and the time since the newest block.

- Solve times far over 300 s with difficulty still rising: a fault in the
  difficulty rule (this stalled the version 2 chain). Compare with the
  simulations in `jcoin/src/chain.rs`.
- Long gap, difficulty steady: hash power left. Since 2.2.0 the next block
  eases after 20 minutes; expected time for one miner is
  `2^bits / hash rate` seconds.
- Many blocks on forks: two groups mined apart; check versions and peers.

## 3. Another person's node

Use the `sentry-logs` skill. The questions and the events that answer them:

| Question | Look for |
| --- | --- |
| Which version and chain is it on | `startup`, `store_replayed`, `chain_moved_aside` |
| Can others reach it | `reservation_ok` then `rendezvous_registered`; `reservation_lost` |
| Did it connect to a given node | `peer_connected` naming that peer id, on both sides |
| Is it mining | `mining_started`, `block_won` |
| Does it see newer rules | `newer_rules` |

## 4. The relay

`tools/relay-log 2h [PEER]`. A node that is reachable shows `reservation for`
and `rendezvous registration from`; `circuit A -> B` is A dialing B through
the relay. A node that only appears as the source of circuits is not
reachable itself.

## Causes seen before

- **Different chains.** Nodes on different versions connect but refuse each
  other's blocks. Check `startup` versions. The iMac (`miner@jcoin-miner.local`,
  home network only) runs whatever was last deployed to it.
- **No relay reservation** (fixed in 2.1.1): the node dials out but nobody
  can find it.
- **Transfer not in a block.** Before 2.1.2 a node that connected after the
  transfer was sent never heard of it. Pending transfers are also lost when
  the sender's node restarts (still open). The sender's confirmed `nonce`
  rising past the transfer's nonce is the proof that it landed.
- **Clock.** A computer more than 10 minutes slow refuses current blocks
  ("timestamp too far in the future").
- **Two copies of the app** on one data folder, or a development build run
  against the real data folder: it can refuse stored blocks and mine a
  private fork. Check `startup` lines and running processes.
- **Windows direct connections** often fail; such nodes stay on the relay,
  which works.

## Reporting

Give the answer first, then the evidence in a few lines with times in UTC.
Say what you could not see (for example a node that sends no logs).
