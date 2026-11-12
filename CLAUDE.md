# JeffyCoin — guide for Claude

Jonathan's hobby proof-of-work cryptocurrency in Rust: a node, a wallet, a GPU
miner, a relay, and a desktop app that combines them. Friends run the app on
Windows. It is a learning project; he reviews the design and decides
consensus questions himself.

## Rules

- Never run `cargo fmt` (the code is not rustfmt-formatted).
- No Claude attribution in commits or pull requests.
- "Commit" means a local commit. Push, merge, tag, and delete remote branches
  only when Jonathan says so. Work on a descriptive branch, never on `main`.
- A release is a `v*` tag on `main`. Use the `release` skill.
- Before building a change, add it as an `[[item]]` in `planned.toml` (format
  in the file's header). Edit single fields; never write version numbers
  there (`tools/planned` works them out). When a release ships, write its
  `CHANGELOG.md` section and delete its release and items from the file.
- A consensus change (anything that alters which blocks are valid) needs his
  decision, a higher block version, and a `2.x.0` release. There are no
  required or forced updates; the app only advises.
- Write plain technical English: short sentences, no hype.
- When updating context files (guides, handoff, planned changes, known
  issues), rewrite them to state only what is true now. Delete outdated text
  and finished reminders; do not add a note that supersedes them. History
  belongs only where it is the point, such as a post-mortem or a decision
  record.

## Where things are

| Path | What | Read first |
| --- | --- | --- |
| `jcoin/` | Node, wallet, relay, consensus, peer-to-peer | `jcoin/CLAUDE.md` |
| `app/` | Desktop app (egui): node + wallet + miner, install, updater, reporting | `app/CLAUDE.md`, `app/README.md` |
| `gpu-miner/` | GPU miner library and program (wgpu) | `gpu-miner/README.md` |
| `deploy/` | Relay deployment (AWS Lightsail) | `deploy/README.md` |
| `tools/` | Scripts for routine work (below) | each script's `--help` |
| `docs/adr/` | Decision records | |
| `docs/HANDOFF.md` | What the last session left open | at the start of a session |
| `docs/KNOWN-ISSUES.md` | Known weaknesses | when touching validation or peers |
| `demo/` | Old local regtest demo scripts | rarely needed; prefer `tools/local-net` |
| `spike/` | Frozen scratchwork | never |

Design notes are in the Obsidian vault
(`../Chudjacknostim-Reorganized/Projects/JeffyCoin/`), which has its own
strict `CLAUDE.md`.

## Use the scripts

Run these in place of composing commands. Each prints a few lines.

| Task | Command |
| --- | --- |
| Planned releases, numbered | `tools/planned` (`show ID\|VERSION\|backlog\|ops`, `check`) |
| Build and test everything | `tools/check` (`--gpu` adds the GPU tests, `--build` builds programs; failures saved in `.check-logs/`) |
| Is a failure new? | `tools/check --at main [PACKAGE]` (same checks on another commit) |
| Is an intermittent test fixed? | `tools/repeat-test PACKAGE FILTER [N] [--ignored]` |
| Release a version | `tools/release 2.2.1 --yes` (only when told; see the `release` skill) |
| State of a local node or the app | `tools/node-status [PORT] [--peers] [--blocks N]` |
| Wait for a node state | `tools/node-status PORT --wait direct\|peers=N\|height=N [SECS]` |
| Read a chain file as a table | `tools/chain-dump [PATH] [--tail N]` |
| Compare GPU kernels and batch sizes | `tools/gpu-bench [SECS] [--adapter N]` (stop mining first) |
| Who mined, and every transfer | `tools/chain-dump --who [--not ADDR,...]` |
| Update the installed app on this laptop | `tools/update-app [VERSION] --yes` (checks checksum and glibc; mining must be restarted in the window) |
| Throwaway local nodes | `tools/local-net up [N] [mine] [relay]`, `status`, `peers`, `down` |
| Picture of the app window | `tools/app-shot [--dev] [--tall] [--mainnet]` |
| Friends' logs (Sentry) | the `sentry-logs` skill, `tools/sentry-logs` |
| Diagnose one node's logs in one call | `tools/sentry-logs diagnose NODE` (also `status`, `peers`) |
| The relay's log | `tools/relay-log [SINCE] [PEER]` |
| Stall simulations | `cd jcoin && cargo test --release stall_report -- --ignored --nocapture` |

## Keeping token use low

- Do not print build or test logs. `tools/check` shows failures only.
- Do not improvise a diagnosis. Starting processes by hand, `curl` plus a
  JSON one-liner, waiting loops, or `git stash` to compare with `main` all
  mean a script is missing: extend one in `tools/` first, then use it.
- Do not read a whole source file to find one thing: the per-folder guides
  list what each file holds; search for the function, then read that part.
- Edit with the edit tool. Do not rewrite files through scripts.
- Stop a test process by its process id. `pkill -f PATTERN` also kills the
  shell that runs it when the pattern is in its own command line.
- If a command is needed a second time, add it to `tools/` and to the table
  above, with a `--help`, compact output, and a non-zero exit on failure.
- Put context next to what it describes: a folder's `CLAUDE.md` for how to
  work in it, its `README.md` for what it does. Keep this file an index.
- Start Claude Code in this repository, not in the vault, so this guide and
  the skills load and the vault's guide does not.

## What cannot be checked here

- Windows. The Windows build is compiled only by the release workflow. The
  first real test of a Windows change is a friend's computer; read the result
  with the `sentry-logs` skill.
- Clicking in the app window. Test behaviour through `app/src/backend.rs`
  tests; use `tools/app-shot` for layout.
