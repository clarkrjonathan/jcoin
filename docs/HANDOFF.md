# JeffyCoin — session handoff

What a new session needs that the code and changelogs do not say. Keep it
short: released changes go in `../CHANGELOG.md`, planned work and its causes
in `../planned.toml`, and how the repository is organized in
`../CLAUDE.md`. Rewrite the section below at the end of a session so that it
states only what is true now.

## Current state

**Releases**

- Latest release: `v2.2.4`. Friends run 2.2.x on Windows. The chain is the
  one started by 2.2.0 (block version 3).
- Planned releases: `tools/planned`. Settled consensus questions are in
  `docs/adr/0004-difficulty-and-stall-fallback.md`.
- Work goes on a descriptive branch, is merged into `main`, and a `v*` tag on
  `main` starts the release build. The tag must equal the version in
  `app/Cargo.toml`.
- Decided by Jonathan: **no required updates.** The app never refuses to run
  or mine because of its version; it only prompts harder from network
  evidence.

**What exists**

- Desktop app `app/` (`jcoin-app`): node + wallet + GPU miner, keys, GPU use
  limit, Network and Developer sections, updater, per-user install, Sentry
  reporting.
- `jcoin::service` (embeddable node), `jcoin::wallet`, `jcoin::log`.
- Sentry: project `iowa-state-pe/jcoin-app`. Read logs with the
  `sentry-logs` skill and `tools/sentry-logs`. The reader needs a user token
  in `~/.config/jeffycoin/sentry-token`; none exists yet, and its request to
  Sentry is untested.
- Relay: AWS Lightsail `3.14.123.27:9000`, ssh `ubuntu@3.14.123.27` with
  `~/jcoin-relay.pem`. It runs the pre-2.1.1 relay program.
- iMac reference node: `miner@jcoin-miner.local`, LAN only. It runs a build
  from before 2.2.0 and cannot sync with the current chain.

**Pending operations**: `tools/planned show ops`.

**Working in this repository**

- `CLAUDE.md` is the index: rules, where things are, and the scripts in
  `tools/`. Each package has its own `CLAUDE.md` (file map, how to test) and
  `README.md` (what it does).
- Skills in `.claude/skills/`: `release`, `verify`, `diagnose-network`,
  `sentry-logs`. They load when Claude Code is started in this repository.
