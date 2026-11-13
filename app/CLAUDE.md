# app package — working notes

The desktop app. What it does, its files on disk, and its settings are in
`README.md` in this folder. `tools/check app` runs its tests; `--gpu` adds the
end-to-end test (mine, send, confirm on a private regtest chain).

## Files

| File | Holds |
| --- | --- |
| `src/main.rs` | The window: one function per section (`network_section`, `wallet_section`, `keys_section`, `mining_section`, `send_section`, `updates_section`, `developer_section`), the banners (`newer_rules_banner`, `install_banner`), `Updates`, `Install` |
| `src/backend.rs` | Everything behind the window: `Settings` (environment overrides), `Backend` (keys, sending, mining, GPU limit, diagnostics), the node thread, the poller that reads the node's REST endpoints into `NodeView` |
| `src/install.rs` | Per-user install: `Layout`, menu entry, uninstall |
| `src/update.rs` | Release list, checksum check, replacing the program file |
| `src/recover.rs` | Restart after a crash while mining: panic hook, `--after-crash`, waiting for the old copy |
| `src/telemetry.rs` | Sentry setup, tags, `classify` (log line to `component` and `event`) |
| `src/applog.rs` | The log file and the in-memory tail |

## How it fits together

- The window never blocks. Background threads write shared state; the window
  reads a copy each frame and redraws every 500 ms.
- The app talks to its own embedded node over local REST, like the
  command-line tools. To show something new, add it to the node's REST reply,
  read it in the poller (`spawn_poller`), put it in `NodeView`, draw it.
- Behaviour is tested through `Backend` (`backend.rs` tests), not through the
  window. Layout is checked with `tools/app-shot`.
- A new kind of log line gets its `event` by adding a rule and a test case in
  `telemetry::classify`.

## Constraints

- eframe is pinned to 0.35: later versions need a `js-sys` that conflicts
  with libp2p 0.57's pin.
- The version in `Cargo.toml` must equal the release tag; the updater compares
  them.
- Windows-only paths (`install.rs` shortcut through PowerShell, console
  hiding) cannot be run here. Keep them small and say so when changing them.
- Test settings: `JCOIN_APP_REGTEST=1`, `JCOIN_APP_DATA`, `JCOIN_APP_RELAY`,
  `JCOIN_APP_PEER`, `JCOIN_APP_NO_TELEMETRY=1`, `JCOIN_APP_DEV=1`,
  `JCOIN_APP_NO_INSTALL=1`. Always set `JCOIN_APP_NO_TELEMETRY=1` and a
  separate data folder for a test run, or use `tools/app-shot`.
