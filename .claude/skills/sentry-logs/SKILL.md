---
name: sentry-logs
description: Read the JeffyCoin desktop app's Sentry logs without spending many tokens. Use when asked what a friend's node did, whether nodes connected, why a node is unreachable, to "pull the logs", or to check a release in the field. Covers the compact command-line reader (tools/sentry-logs) and, when it has no token, how to query the Sentry MCP tools cheaply.
---

# Reading JeffyCoin's Sentry logs

The desktop app (2.1.1 and later) sends every log line to Sentry project
`iowa-state-pe/jcoin-app`. Each line has attributes: `component`, `event`,
`device`, `install_id`, `peer_id`, `network`, `address`, `gpu`, `installed`
(see `app/README.md`, "Reporting").

A raw log query is expensive: one failed-dial line is 300 to 800 characters,
a node logs dozens on every start, and the MCP tool adds an id and two links
to every row. Follow the order below. Each step is cheap and tells you whether
the next one is needed.

## The rule

1. **Count before you read.** Start with counts by node, then counts by
   `event` for the node in question.
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
tools/sentry-logs nodes -p 2h              # who reported: device, install id, OS, release, count, last seen
tools/sentry-logs events NODE -p 2h        # count of each event for one node
tools/sentry-logs timeline NODE -p 2h      # the story: startup, reservation, peers, mining, blocks
tools/sentry-logs lines NODE -e EVENT      # only chosen events
tools/sentry-logs lines NODE --errors      # errors and warnings, without routine noise
tools/sentry-logs query 'SENTRY QUERY' -f timestamp -f event   # anything else
```

`NODE` is a device label, a 16-hex install id, a peer id, or the 6 characters
after `12D3KooW`. `all` means every node. Default period is 2 hours; keep it
as short as the question allows. Lines print as
`HH:MM:SS <E|W| > event message` in UTC.

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
   `query: "install_id:<id> event:[startup,reservation_ok,reservation_lost,rendezvous_registered,peer_connected,peer_disconnected,hole_punch_ok,hole_punch_failed,mining_started,mining_stopped,block_won]"`,
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
| `mining_started` / `mining_stopped` / `gpu_limit_set` | Mining controls |
| `block_won` / `block_accepted` | This node mined a block |
| `dial_failed`, `stream_failed`, `listening`, `block_candidate` | Routine; noise for most questions |

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

## Token

The reader needs a Sentry **user** auth token with scopes `org:read`,
`project:read`, and `event:read`, in the environment variable
`SENTRY_READ_TOKEN` or the file `~/.config/jeffycoin/sentry-token`. An
organization token (the kind used for uploading debug files) cannot read
events. Never write the token into the repository.

`tools/sentry-logs --self-test` checks the text handling without a token.
