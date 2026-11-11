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
   controls to mine on a GPU and to send coins.

## Keys

The Keys section lists every key file in the data folder (`<name>_ed25519`, the
same format the `wallet` command-line tool writes). It can create a key, switch
the key in use, copy the secret key of the key in use for a backup, and import a
key from its secret key. The key in use is recorded in the file `active_key`.
Switching is refused while mining, because a running miner pays the address it
was started with. There is no delete: remove a key file by hand.

Mining rewards go to the address of the key in use. A sent transfer shows
"confirmed" once the wallet's confirmed nonce has passed the transfer's nonce.

## Developer section

Collapsed by default (`JCOIN_APP_DEV=1` starts with it open). It shows:

- The app version and the tip's difficulty in leading zero bits.
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

## Structure

- `jcoin::service` (in `jcoin/`) is the node as an embeddable service. The
  `node` binary and the app both call `service::run`.
- `jcoin::wallet` holds key files and transaction signing. The `wallet` binary
  and the app both use it.
- `jcoin_gpu_miner::miner` (in `gpu-miner/`) is the mining loop, with a
  `MinerControl` to stop it and read the hash rate.
- `app/src/backend.rs` runs those on background threads and talks to the
  embedded node over its local REST endpoints. `app/src/main.rs` is the window
  (egui through eframe).

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
