---
name: verify
description: Check a JeffyCoin change with the fewest steps and least output. Use after editing code in jcoin/, app/, or gpu-miner/, before committing, or when asked whether something works. Says which script covers which kind of change and what cannot be verified on this computer.
---

# Verifying a change

Pick the row that matches what changed. Run the script; do not compose the
commands by hand and do not print build logs.

| Changed | Run | Notes |
| --- | --- | --- |
| Anything | `tools/check` | One line per package; failures only in detail |
| Mining, the miner, app mining or sending | `tools/check --gpu` | Mines, sends, and confirms on a private regtest chain |
| The GPU kernel or `gpu.rs` | `gpu-miner/target/release/gpu-miner --self-test` | After `tools/check gpu-miner --build` |
| The difficulty rule | `tools/check jcoin`, then the stall report | `cd jcoin && cargo test --release stall_report -- --ignored --nocapture` |
| Peer-to-peer or sync | `tools/local-net up 3`, then `tools/local-net status` | Regtest, fixed difficulty; `down` when finished |
| App layout | `tools/app-shot --dev --tall`, then read the picture | Add `--mainnet` to see the install box |
| A node's REST reply | `tools/node-status PORT` or one `curl` | |
| A chain file | `tools/chain-dump PATH --tail 20` | |

## What each result means

- `tools/check` exit 0 and "N passed" for each package is the bar for a
  commit. A warning about `ancestor` being unused in `chain.rs` is old and
  expected.
- The GPU test needs a free GPU: stop other miners on this computer first.
- Regtest has no difficulty adjustment. To see the real rule run, start a
  node without `--regtest` on a temporary data folder with `--no-listen`, and
  mine it with the GPU miner; watch with `tools/node-status PORT`.

## What cannot be verified here

- **Windows.** Only the release workflow compiles it. Say so in the report,
  and confirm from a friend's logs after the release.
- **Clicks in the app window.** Test the behaviour through `Backend` in
  `app/src/backend.rs`; use the picture for layout only.
- **The public relay path between two home networks.** A local relay
  (`jcoin/target/release/relay --addr 127.0.0.1:9100 --key-file KEY`) covers
  the logic; the real path needs two networks.

## Reporting

State what was run and what it showed, what was not run, and what could not
be. Do not write "tested" for something only compiled.
