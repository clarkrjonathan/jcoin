---
name: sentry-logs
description: Read the JeffyCoin desktop app's Sentry logs without spending many tokens. Use when asked what a friend's node did, whether nodes connected, why a node is unreachable, to "pull the logs", or to check a release in the field. Covers the compact command-line reader (tools/sentry-logs) and, when it has no token, how to query the Sentry MCP tools cheaply.
---

# Reading JeffyCoin's Sentry logs

The desktop app (2.1.1 and later) sends every log line to Sentry project
`iowa-state-pe/jcoin-app`. Each line has attributes: `component`, `event`,
`device`, `install_id`, `run`, `peer_id`, `network`, `address`, `gpu`,
`installed` (see `app/README.md`, "Reporting"). Since 2.2.2 a line may also
carry its own fields: `run` (one program start), `peer` (6 chars after
`12D3KooW`), `reason` (a failure class), `via` (`relay`/`direct`), `height`,
and the status fields. Query by these instead of reading message text.

A raw log query is expensive: one failed-dial line is 300 to 800 characters,
a node logs dozens on every start, and the MCP tool adds an id and two links
to every row. Follow the order below. Each step is cheap and tells you whether
the next one is needed.

## The rule

0. **Ask `diagnose` first.** `tools/sentry-logs diagnose NODE` answers "what is
   wrong with this node" in one call (current state, peers, signals, recent
   errors) with no raw lines. Start here; only drop to the steps below when it
   points somewhere specific.
1. **Count before you read.** Counts by node (`nodes`), then by `event` for the
   node in question (`events NODE`).
2. **Read the story, not the log.** Ask only for the events that answer the
   question. Never fetch `dial_failed` lines unless the question is about one
   specific peer that cannot be dialed.
3. **Stop when the question is answered.** Do not page through the rest.

## With the command-line reader (preferred)

`tools/sentry-logs` in the jcoin repository prints compact text: peer ids
become `<6 chars>`, addresses become `host:port`, repeated lines collapse to
one line with `(xN)`. It needs a token (see "Token" below). If it prints
"No Sentry token", use the MCP method instead; do not ask the user for a token
unless they want the cheaper path.

```
tools/sentry-logs diagnose NODE -p 2h      # one digest: state, peers, signals, errors (start here)
tools/sentry-logs status NODE -p 2h        # the latest status line (height, peers, mining, pending, balance)
tools/sentry-logs peers NODE -p 2h         # who it connected to, by peer and transport
tools/sentry-logs nodes -p 2h              # who reported: device, install id, OS, release, count, last seen
tools/sentry-logs events NODE -p 2h        # count of each event for one node
tools/sentry-logs timeline NODE -p 2h      # the story for the latest run (--all-runs for every run)
tools/sentry-logs lines NODE -e EVENT      # only chosen events
tools/sentry-logs lines NODE --errors      # errors and warnings, without routine noise
tools/sentry-logs query 'SENTRY QUERY' -f timestamp -f event   # anything else
```

`NODE` is a device label, a 16-hex install id, a peer id, or the 6 characters
after `12D3KooW`. `all` means every node. Default period is 2 hours; keep it
as short as the question allows. `timeline` and `lines` default to the node's
latest `run`. Lines print as `HH:MM:SS <E|W| > event message` in UTC.

## With the Sentry MCP tools (no token)

Use `mcp__plugin_sentry_sentry__search_logs` with
`organizationSlug: "iowa-state-pe"`, `projectSlug: "jcoin-app"`. Always pass
`fields` and `limit`; pass the query in Sentry syntax, not prose, so no
translation step guesses.

1. Who reported (one cheap aggregate):
   `query: ""`, `fields: ["device","install_id","os.name","sentry.release","count()"]`,
   `sort: "-count()"`, `limit: 20`.
2. What kinds of lines one node has:
   `query: "install_id:<id>"`, `fields: ["event","severity","count()"]`, `limit: 40`.
3. The story for that node. Leave `message` out when the event name is enough:
   `query: "install_id:<id> event:[startup,reservation_ok,reservation_lost,rendezvous_registered,peer_connected,peer_disconnected,hole_punch_ok,hole_punch_failed,relay_replaced,mining_started,mining_stopped,block_won]"`,
   `fields: ["timestamp","event","message"]`, `sort: "timestamp"`, `limit: 30`.
4. Only if needed, errors without the noise:
   `query: "install_id:<id> severity:[error,warn] !event:dial_failed !event:stream_failed !event:hole_punch_failed"`,
   `fields: ["timestamp","event","message"]`, `limit: 15`.

Rules for MCP queries:

- `os.name` values are lowercase for Windows (`windows`) and `Linux` for
  Linux. Prefer `install_id` or `device` to select a node.
- Do not run a query without an `event` or `severity` filter and with
  `message` in `fields`. That is the query that returns pages of failed dials.
- Keep `limit` at 30 or below; raise it only after a count shows it is needed.
- Use `period` no longer than the question needs (`30m`, `2h`).
- Do not paste result rows back to the user. Summarize, and give the Sentry
  link from the result if they want the detail.

## What the events mean

| Event | Meaning |
| --- | --- |
| `startup` | The app started; the line has version, network, OS, key address |
| `install` | Whether this is the installed copy, and its path |
| `store_replayed` | Blocks loaded from disk and the tip height |
| `identity` | The node's peer id |
| `reservation_ok` | The relay accepted the reservation: the node can be reached |
| `reservation_lost` | The reservation closed or could not be requested: the node cannot be reached until `reservation_ok` appears again |
| `rendezvous_registered` | Registered for discovery; other nodes can find it |
| `peer_connected` / `peer_disconnected` | A protocol session with another node started or ended. `(out)` means this node dialed |
| `hole_punch_ok` / `hole_punch_failed` | A direct connection was or was not made; on failure the relay carries the traffic |
| `relay_replaced` | A direct connection to a peer opened (LAN address or hole punch), so its relayed connection was closed; the session moves to the direct one (2.2.3+) |
| `mining_started` / `mining_stopped` / `gpu_limit_set` | Mining controls |
| `block_won` / `block_accepted` | This node mined a block |
| `status` | Once a minute: height, peers, reservation, mining, hash_rate, gpu_limit, pending, balance (see `status`/`diagnose`) |
| `block_received` / `sync_done` | A block arrived from a peer; a sync caught up (has `height`) |
| `reorg` | The tip moved to another branch (has `height`, `depth`) |
| `transfer_sent` / `transfer_confirmed` / `transfer_received` | A transfer this node sent, confirmed, or received |
| `pending_dropped` | A pending transfer left the mempool without confirming |
| `update_started` / `update_installed` / `update_failed`, `installed` / `uninstalled` | Update and install flow |
| `update_check_failed` / `update_check_limited` | The release list could not be read; or GitHub's API refused it (rate limit) and the releases feed was read instead (2.3.2+) |
| `dial_failed`, `stream_failed`, `listening`, `block_candidate` | Routine; noise for most questions. A repeated failed dial (same `peer` + `reason`) reaches Sentry once per `run` |

## Common questions

- **Did two nodes connect?** On each node, `peer_connected` naming the other's
  peer id. Compare the times. One side `(out)`, the other `(in)`.
- **Why can nobody reach a node?** It needs `reservation_ok` and then
  `rendezvous_registered` after its `startup`. If `reservation_ok` is missing
  or followed by `reservation_lost`, that is the cause.
- **Is a node on the right version and chain?** `startup` (version, network)
  and `store_replayed` (tip height). Two nodes on the same chain report the
  same height once synced.
- **Did a release reach people?** `nodes` shows `sentry.release` per node.
- **What state is a node in?** `status NODE` (or `diagnose NODE`): the latest
  `status` line gives height, peers, reservation, mining, hash_rate, pending,
  and balance in one row, without reading the history.
- **Is the chain stuck or reorging?** `diagnose` lists `reorg` and
  `reservation_lost` signals; compare `status` height across nodes.

## Token

The reader needs a Sentry **user** auth token with scopes `org:read`,
`project:read`, and `event:read`, in the environment variable
`SENTRY_READ_TOKEN` or the file `~/.config/jeffycoin/sentry-token`. An
organization token (the kind used for uploading debug files) cannot read
events. Never write the token into the repository.

`tools/sentry-logs --self-test` checks the text handling without a token.
