# Desktop app

`app/` builds `jcoin-app`, one program that contains the node, the wallet, and
the GPU miner. A release publishes it as `JeffyCoin-windows-x86_64.exe` and
`JeffyCoin-linux-x86_64`. It needs no installer and no other file.

## What it does when opened

1. Creates the data folder: `%APPDATA%\JeffyCoin` on Windows,
   `~/.local/share/jeffycoin` on Linux.
2. Creates the wallet key `wallet_ed25519` there on first run. The key file is
   not encrypted. Whoever has it can spend the coins, and there is no recovery
   if it is lost.
3. Starts a node in the same process, on the real network, connected through
   the public relay. The chain is stored in the data folder.
4. Shows the connection status, block height, balance, and address, with
   controls to mine on a GPU and to send coins. The Network section also
   shows the time since the last block, the average block time over the last
   30 blocks, and a "Recent blocks" table (height, age, time after the block
   before, difficulty, miner). While mining, the Mining section shows the
   expected time for this computer to find a block if it mined alone.

## Installing

`app/src/install.rs`. The download is a single file with a long name. A copy
that is not installed shows an "Install JeffyCoin" box. Installing copies the
program to a fixed place under a short name, adds a menu entry, and starts the
installed copy. The download is left in place. No administrator rights are
needed.

| | Windows | Linux |
| --- | --- | --- |
| Program | `%LOCALAPPDATA%\Programs\JeffyCoin\JeffyCoin.exe` | `~/.local/bin/jeffycoin` |
| Menu entry | `Start Menu\Programs\JeffyCoin.lnk` | `~/.local/share/applications/jeffycoin.desktop` |
| Icon | none yet | `~/.local/share/icons/hicolor/scalable/apps/jeffycoin.svg` |
| Installed version | `version.txt` next to the program | `~/.local/share/jeffycoin-install/version.txt` |

- The Start menu shortcut is made by running PowerShell (a `.lnk` file can
  only be made through the Windows shell).
- Updates apply to the installed copy only; a copy that is not installed has
  the update button disabled.
- If the app is already installed, a downloaded copy offers to open the
  installed app, or to replace it when the versions differ.
- The installed copy has "Uninstall" at the bottom of the window. It keeps the
  data folder.
- The install box is hidden on the test network, when `JCOIN_APP_DATA` is set,
  and when `JCOIN_APP_NO_INSTALL=1` is set.

## Keys

The Keys section lists every key file in the data folder (`<name>_ed25519`, the
same format the `wallet` command-line tool writes). It can create a key, switch
the key in use, copy the secret key of the key in use for a backup, and import a
key from its secret key. The key in use is recorded in the file `active_key`.
Switching is refused while mining, because a running miner pays the address it
was started with. There is no delete: remove a key file by hand.

Mining rewards go to the address of the key in use. The Mining section has a
GPU use limit (10% to 100%): the miner rests between GPU dispatches so the GPU
is busy for about that share of the time. It can be changed while mining and is
stored in the file `gpu_limit`. The "Sent" list holds the last 100 transfers
sent from the app, stored in `sent.txt`. A sent transfer shows "confirmed"
once the wallet's confirmed nonce has passed the transfer's nonce. The node
keeps pending transfers in `pending.dat`, so they are still pending after a
restart.

## Developer section

Collapsed by default (`JCOIN_APP_DEV=1` starts with it open). It shows:

- The app version and the tip's difficulty in leading zero bits.
- The GPU kernel the miner runs: `baseline` (default), `unrolled`, or
  `precomputed` (see `gpu-miner/README.md`). The choice is stored in the file
  `gpu_kernel`; changing it while mining restarts mining with it.
- Every open connection from `GET /peers`: peer id, whether it is direct,
  through the relay, or the relay itself, its direction, whether blocks are
  exchanged over it, and its age. The peer's chain height is not shown; the
  protocol's handshake does not carry it.
- The blocks mined since the app started: height, time, reward, transfer
  count, and hash.
- The end of the log. The whole log is the file `jeffycoin.log` in the data
  folder (node, miner, and app lines, UTC). A log over 2 MB is renamed to
  `jeffycoin.log.old` at the next start.
- "Copy diagnostics": the state, connections, mined blocks, and last 200 log
  lines as text. It holds addresses but no secret keys.

The library logs through `jcoin::log` (`log_info!`, `log_error!`), which prints
as before and passes each line to a sink the app sets.

## Reporting (Sentry)

The app sends every log line to Sentry Logs and reports crashes, so problems on
another person's computer can be read without asking for files. The Developer
section says so and lets the user label the computer. `app/src/telemetry.rs`
holds all of it.

Each log line carries attributes for sorting and filtering in Sentry:

| Attribute | Meaning |
| --- | --- |
| `component` | The part that logged: `app`, `p2p`, `work`, `gpu`, `store` |
| `event` | The kind of line, for example `dial_failed`, `reservation_ok`, `reservation_lost`, `peer_connected`, `block_won`, `block_accepted`, `startup` |
| `device` | The label set in the Developer section (also `user.name`) |
| `install_id` | A random id made on first run, stored in the data folder (also `user.id`) |
| `run` | A fresh id made at each program start; selects one run without a time window |
| `peer_id` | The node's libp2p peer id; it matches the relay's log |
| `network` | `mainnet` or `regtest` (also the Sentry environment) |
| `address` | The address of the key in use |
| `gpu` | The GPU in use while mining |

A line may also carry its own fields, so a query can filter on them instead of
reading the message text: `peer` (6 chars after `12D3KooW`), `reason`
(`timeout`, `refused`, `no_reservation`, `address_in_use`, `handshake`,
`other`), `via` (`relay` or `direct`), `height`, and the `status` fields
(`peers`, `reservation`, `mining`, `hash_rate`, `gpu_limit`, `pending`,
`balance`). A `status` line is logged once a minute. A repeated failed dial
(same `peer` and `reason`) is written to the local log file every time but sent
to Sentry only once per `run`.

Example searches in Sentry Logs: `device:"Noah desktop" component:p2p`,
`event:reservation_lost`, `event:dial_failed peer:AbC123`, `event:status`.

Failed dials, failed protocol streams, and failed hole punches are routine on a
peer-to-peer network; they are sent as warnings, not errors. Everything else the
node logs as an error is sent as an error.

`telemetry::classify` maps a line's text to `component` and `event`. A new kind
of log line gets its own `event` by adding a rule there (and a case to its
test). `telemetry::set_tag` adds a tag to all later lines and crash reports.

To read the logs from a terminal as compact text, use `tools/sentry-logs`
(`tools/sentry-logs --help`). The `sentry-logs` skill in `.claude/skills/`
describes how to read them in the fewest steps.

- `JCOIN_APP_NO_TELEMETRY=1` turns reporting off.
- `JCOIN_APP_TELEMETRY_DEBUG=1` prints what the SDK sends to stderr.
- Secret keys are never logged. Addresses, peer ids, and IP addresses in log
  lines are sent.
- The release workflow uploads the app's debug files to Sentry when the
  repository secret `SENTRY_AUTH_TOKEN` is set, so crash reports show function
  names and lines. Without the secret the step is skipped.

## Relay reservation

A node behind NAT can be found and dialed only while it holds a reservation on
the relay. The host connects to the relay, requests the reservation on that
connection, and requests it again every tick if it is lost; a loss is logged as
`relay reservation lost`. On Windows, outgoing connections do not reuse the
listen port (that fails there with os error 10048), so hole punching is not
expected to work from Windows; such nodes stay on the relay.

## Updates

The app reads the GitHub release list of the repository named in
`app/src/update.rs` at startup and on "Check again". It lists the releases that
have a build and a checksum list for this system. "Install and restart"
downloads the chosen release, checks its SHA-256 against the release's list,
renames the running program to `<name>.old`, puts the download in its place,
and starts it. The checksum catches a damaged download; it does not prove who
built the file.

The release list must be readable without signing in, so the repository (or a
separate releases repository) must be public. The app's version in
`app/Cargo.toml` must equal the release tag without the `v`; the release
workflow checks this, and builds only for a tag whose commit is on `main`.

## Local API

The app serves an HTTP API on 127.0.0.1 with what the window shows and does:
status, keys (create, import, select, read the secret), sending and the sent
list, mining (start, stop, GPU, limit, kernel, blocks mined), updates and
install, the computer's label, diagnostics, the log, restart, and the node's
chain, blocks, peers, balances and transactions passed through from its REST
server. It listens only on 127.0.0.1, port 3100 or another free one.

At start the app writes two files to the data folder: `api_port` (the port)
and `api_token` (64 hex characters, created once and kept; readable by this
user only on Linux). Every request under `/api` needs
`Authorization: Bearer <token>`. The token stops web pages in a browser from
driving the app; it does not protect against programs run by the same user,
which can read the key files anyway.

`/docs` is a Swagger page for reading and trying the API (press Authorize and
paste the token); `/openapi.json` is the OpenAPI 3.1 document. Neither needs
the token. From this repository, `tools/app-api` calls the API and
`tools/node-status` reads the app's node through it.

Actions that end the program (`/api/restart`, `/api/install`,
`/api/uninstall`, `/api/updates/install`) reply first with 202 and act half a
second later. Mining does not resume after a restart.

## Structure

- `jcoin::service` (in `jcoin/`) is the node as an embeddable service. The
  `node` binary and the app both call `service::run`.
- `jcoin::wallet` holds key files and transaction signing. The `wallet` binary
  and the app both use it.
- `jcoin_gpu_miner::miner` (in `gpu-miner/`) is the mining loop, with a
  `MinerControl` to stop it and read the hash rate.
- `app/src/backend.rs` runs those on background threads and talks to the
  embedded node over its local REST endpoints. `app/src/main.rs` is the window
  (egui through eframe). `app/src/api.rs` is the local API; it shares the
  backend with the window.

eframe is pinned to 0.35: later versions need a `js-sys` version that conflicts
with the one libp2p 0.57 pins.

## Test settings

Environment variables override the defaults:

| Variable | Effect |
| --- | --- |
| `JCOIN_APP_REGTEST=1` | Use the local test network and a separate data folder |
| `JCOIN_APP_DATA` | Data folder |
| `JCOIN_APP_RELAY` | Relay multiaddr, or `none` |
| `JCOIN_APP_PEER` | A peer to dial at startup |

`cargo test --release -- --ignored` in `app/` runs an end-to-end test on a
private regtest chain: mine a block on the GPU, send coins, see the transfer
confirmed. It needs a GPU.

## Known limits

- The Windows program is unsigned, so SmartScreen warns on first run.
- Node log output is not shown in the window and is not written to a file.
- Mining uses a GPU only. There is no CPU fallback in the app.
