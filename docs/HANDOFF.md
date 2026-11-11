# JeffyCoin — session handoff

What a new session needs that the code and changelogs do not say. Keep it
short: released changes go in `../CHANGELOG.md`, planned work and its causes
in `../CHANGELOG-PLANNED.md`, and how the repository is organized in
`../CLAUDE.md`. Replace the section below at the end of a session; do not
append to it.

## Where the last session (2026-11-11) stopped

Read this first. The sections below it are older and partly out of date; the
current sources are `CHANGELOG.md` (what is released), `CHANGELOG-PLANNED.md`
(what is planned, with causes), and `app/README.md` (the desktop app).

**State of the code**

- Released: `v2.2.0` (tag on `main`): the chain restart. New genesis
  2026-11-11 19:05 UTC, block version 3, corrected difficulty rule (window
  30), stall fallback (target doubles 20 minutes after the parent, then every
  10 minutes), timestamps at most 10 minutes ahead, newer-rules banner.
- Work goes on a descriptive branch, is merged into `main`, and a `v*` tag on
  `main` starts the release build. The tag must equal the version in
  `app/Cargo.toml`. `initial-vertical` is an old branch name, no longer used.
- Decided by Jonathan: **no required updates.** The app never refuses to run
  or mine because of its version; it only prompts harder from network
  evidence.

**After the restart**

- The chain of 2.0.0 to 2.1.2 stalled at height 69 (its difficulty rule
  compounded). It is abandoned. Clients on 2.1.2 or earlier stay on it and
  are not prompted; their owners have to be told to update.
- Next planned: 2.2.1 (logs), 2.2.2 (network and mining view), 2.2.3 (wallet
  and block detail), 2.3 (graphical overhaul). See `CHANGELOG-PLANNED.md`,
  including "Consensus backlog".
- `cargo test --release stall_report -- --ignored --nocapture` in `jcoin/`
  prints the stall simulations the settings were chosen from.

**What exists now**

- Desktop app `app/` (`jcoin-app`): node + wallet + GPU miner, keys, GPU use
  limit, Developer section, updater, per-user install, Sentry reporting.
- `jcoin::service` (embeddable node), `jcoin::wallet`, `jcoin::log`;
  `GET /peers`; pending transfers are re-announced to peers every 60 s.
- Sentry: project `iowa-state-pe/jcoin-app`. Read logs with the
  `sentry-logs` skill (`.claude/skills/sentry-logs/`, linked into
  `~/.claude/skills/jcoin-sentry-logs`) and `tools/sentry-logs`. The reader
  needs a user token in `~/.config/jeffycoin/sentry-token`; none exists yet,
  and its request to Sentry is untested.
- Relay: AWS Lightsail `3.14.123.27:9000`, ssh `ubuntu@3.14.123.27` with
  `~/jcoin-relay.pem`. It runs the pre-2.1.1 relay program.

**Not verified on Windows**

- 2.1.2's install flow and Start menu shortcut, and the in-app updater, have
  not been confirmed on a Windows PC. 2.1.1's relay reservation fix was
  confirmed from a friend's logs.

**Pending operations** (also in `CHANGELOG-PLANNED.md`)

- iMac (`miner@jcoin-miner.local`, LAN only) still runs a build with the
  earlier 35-bit genesis; it cannot sync with 2.x nodes. Redeploy from the
  home network.
- Redeploy the relay; add the `SENTRY_AUTH_TOKEN` repository secret; create
  the Sentry read token.

**On the laptop (2026-11-11, afternoon)**

- Two copies of the app were running: the installed one
  (`~/.local/bin/jeffycoin`) and a development build started from
  `app/target/release` during the 2.2.0 work. The development build ran
  against the real data folder with half-finished rules; it refused most
  stored blocks and mined a private fork on the old genesis (height 54). That
  fork is worthless. Both should be closed and the installed app updated to
  2.2.0, which sets the old chain file aside.
- The NVIDIA GPU appears to be on again (about 645 MH/s in a test run).
- The old regtest demo in `demo/` may still be running; `demo/stop.sh` stops
  it. Prefer `tools/local-net` for new tests.

**Working in this repository**

- `CLAUDE.md` is the index: rules, where things are, and the scripts in
  `tools/`. Each package has its own `CLAUDE.md` (file map, how to test) and
  `README.md` (what it does).
- Skills in `.claude/skills/`: `release`, `verify`, `diagnose-network`,
  `sentry-logs`. They load when Claude Code is started in this repository.

**Rules**: in `CLAUDE.md`.

Older handoff sections were removed on 2026-11-11; they described the v1 and
v2 chains. `git log -- docs/HANDOFF.md` has them.
