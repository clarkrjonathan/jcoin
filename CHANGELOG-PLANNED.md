# Planned changes

Changes that are decided or proposed but not released. Released changes are in
[`CHANGELOG.md`](CHANGELOG.md).

How to use this file:

- Add an entry when a problem or a wanted change is found, before any code is
  written. State the problem as observed, then the planned change.
- Put the entry under the version it is planned for, or under "Unscheduled" if
  no version is chosen.
- Mark each entry **Consensus: yes** or **Consensus: no**. A consensus change
  needs every node to update and must be planned around an activation height.
- When the version is released, move its section to the top of
  `CHANGELOG.md`, reworded as what changed, and delete it here.

Status values: `planned` (agreed, not started), `in progress`, `done` (merged
to `main`, waiting for the release).

## 2.1.2

### Share pending transfers with a peer that connects later

- Status: done (not committed to `main` yet)
- Consensus: no
- Found: 2026-11-11. A transfer was sent while two nodes were connected. A
  third node connected afterwards and mined the next block. The block did not
  include the transfer, and the transfer stayed pending on the sender.
- Cause: a node announces a transfer to its peers once, when it first accepts
  it (`NewTransaction`). The protocol has no message that gives a newly
  connected peer the transfers that are already pending. A node that joins
  later never learns of them, so the blocks it mines leave them out.
- Planned change: when a session with a peer starts, send that peer the
  pending transfers (the mempool). Old nodes accept these as ordinary
  `NewTransaction` messages, so old and new versions work together.
- As built: each session sends its peer the pending transfers when it starts
  and again every 60 seconds (at most 256 each time). The repeat is needed: a
  peer that is still syncing rejects a transfer, because it does not know the
  sender's balance yet. A peer that already holds a transfer does not relay
  it, so repeats stop at the first hop.
- Tested: a node that joined after a transfer was sent held it as pending
  within 8 seconds, and a block mined through that node confirmed it.

### A limit on how much of the GPU mining uses

- Status: done (not committed to `main` yet)
- Consensus: no
- Problem: mining runs the GPU without pause. The computer is slow to use
  while it mines, and the GPU runs hot and loud. The only control is to stop
  mining.
- Planned change: a setting in the app's Mining section that limits GPU use,
  as a share of full power (for example 25%, 50%, 75%, 100%). The mining loop
  pauses between GPU dispatches so that the GPU is busy for about that share
  of the time. The setting is remembered between runs and can be changed while
  mining. The command-line miner gets the same limit as an option.
- Effect on mining: the hash rate falls in proportion to the limit, so blocks
  are found less often.
- As built: a slider in the Mining section, 10% to 100% in steps of 5, which
  can be moved while mining and is remembered. The command-line miner has
  `--limit <percent>`, which also applies to `--bench`.
- Measured on an Intel Iris Xe: 152, 113, 71, and 33 MH/s at 100%, 75%, 50%,
  and 25%.
- Not built: lowering the limit by itself while the computer is in use. That
  moves to 2.3 (GPU statistics and modes).

### A consistent install, reachable like any other app

- Status: done (not committed to `main` yet). Not run on Windows.
- Consensus: no
- Problem: there was no install step. The download was the program, and it ran
  from wherever it was saved (often the Downloads folder). It had no Start
  menu or application menu entry and no icon. The updater replaced the file it
  was started from, so what an update did depended on where that copy was.
- As built, on Windows and Linux, for the current user, with no administrator
  rights:
  - A copy that is not installed shows an "Install JeffyCoin" box at the top.
    Installing copies the program to one fixed place under a short name, adds
    a menu entry named "JeffyCoin", and starts the installed copy. The
    download is left where it is, under its long download name.
  - Program: `%LOCALAPPDATA%\Programs\JeffyCoin\JeffyCoin.exe` on Windows,
    `~/.local/bin/jeffycoin` on Linux.
  - Menu entry: a Start menu shortcut on Windows; a `.desktop` file with an
    icon on Linux. Searching "JeffyCoin" finds it.
  - If the app is already installed, a downloaded copy offers "Open the
    installed app" and, when the versions differ, "Replace it with this
    version".
  - Updates apply only to the installed copy. A copy that is not installed
    has the update button disabled, with the reason.
  - The installed copy has a two-step "Uninstall" at the bottom of the window.
    It removes the program and the menu entry and keeps the data folder.
  - The data folder is unchanged (`%APPDATA%\JeffyCoin`,
    `~/.local/share/jeffycoin`) and has an "Open" button next to its path.
- Not done:
  - The Windows program file has no icon of its own, so the Start menu entry
    shows a generic icon.
  - The app does not appear under "Installed apps" in Windows Settings.
  - Nothing stops a downloaded copy and the installed copy from running at
    the same time on the same data folder (see "Unscheduled").

## 2.3

### Graphical overhaul of the desktop app

- Status: planned
- Consensus: no
- Problem: the app is one long page that shows everything to everyone. A
  person who only holds coins sees mining and developer controls; a miner has
  no view of what the GPU is doing.
- Planned change: a redesigned window with modes, so each kind of user sees
  what they need. The working list of modes:
  - **Miner**: mining controls and GPU statistics.
  - **Wallet**: balance, keys, sending, and receiving.
  - **Developer**: connections, blocks, the log, and diagnostics.
- GPU statistics to show: hash rate over time, the GPU use limit, blocks
  found, and, where the system reports them, GPU temperature, power, and load.
- Open points:
  - The exact set of modes and their names.
  - Which GPU figures can be read on each system. wgpu does not report
    temperature, power, or load; they need a separate source for each GPU
    maker and operating system.
  - Whether the GPU use limit lowers itself while the computer is in use
    (carried over from 2.1.2).

## Unscheduled

### Pending transfers are lost when the node restarts

- Consensus: no
- Found: 2026-11-11. Pending transfers are kept in memory only. When the
  sender's app was closed with two transfers pending, its node forgot them.
  They can still confirm if another node holds them; otherwise they must be
  sent again. The app's "Sent this session" list is also lost on restart, so
  the sender cannot see which transfers are affected.
- Possible change: store pending transfers in the data folder and reload them
  at startup; keep the sent list across restarts.

Problems that have been observed and have no version yet.

### Two copies of the app can run on the same data folder

- Consensus: no
- Nothing stops a second copy of the app from starting while one is running.
  Both then write to the same chain file and key folder. Since 2.1.2 a
  downloaded copy and the installed copy make this easier to do by accident.
- Possible change: a lock in the data folder; a second copy brings the first
  one's window forward and exits.

### Nodes on the same local network connect through the relay

- Consensus: no
- Found: 2026-11-11. Two nodes on the same LAN (192.168.0.x) connected to each
  other through the relay. Neither tried the other's LAN address.
- Not yet investigated.

### Log noise from addresses that cannot be reached

- Consensus: no
- Found: 2026-11-11. Nodes pass on loopback and private addresses of other
  machines (`127.0.0.1`, `172.17.0.1`, `::1`) and stale records of old test
  nodes. Each node then tries to dial them and logs a failure. Most log lines
  on a fresh start are these failures.
- Possible change: do not pass on or dial loopback addresses, and drop
  addresses of peers that have not been reachable for some time.

### Difficulty overshoots when a fast miner starts a quiet chain

- Consensus: yes, if the adjustment rule changes
- Found: 2026-11-11. A GPU mined the first 63 blocks in about 30 seconds. The
  difficulty rose to about 38 leading zero bits. Blocks then took far longer
  than the 300-second target for the hash power present, and the difficulty
  comes down only a little with each slow block.
- Not yet investigated. Changing the rule is a consensus change.

### The miner puts at most one transfer in a block

- Consensus: no (the rules already allow 64)
- The node's own miner and the mining endpoints select one pending transfer
  for each block. Planned change: select up to 64, highest fee first, each
  sender's transfers in nonce order.

### Hole punching from Windows

- Consensus: no
- Since 2.1.1, a Windows node dials from a new port, because dialing from the
  listen port fails there (os error 10048). The library's hole-punch dials
  still use the listen port, so a direct connection from Windows is expected
  to fail and the node stays on the relay. A fix would be in how the Windows
  sockets are set up.

### The app does not show a transfer that is on its way in

- Consensus: no
- The app shows only the confirmed balance. A recipient sees nothing until the
  transfer is in a block.

## Operations (no release needed)

- Deploy the 2.1.1 relay program to the Lightsail relay, so that refused
  reservations appear in its log.
- Deploy a current node to the iMac reference node. It still runs a build with
  the earlier 35-bit genesis and cannot sync with 2.x nodes.
- Add the `SENTRY_AUTH_TOKEN` repository secret, so that crash reports show
  function names and line numbers.
