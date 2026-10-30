# JeffyCoin peer-to-peer protocol

Status: implemented and verified by unit and integration tests. Gossip v1 runs
over direct TCP (frame codec, handshake, heartbeat, `NEW_TRANSACTION`), peer
discovery (`GET_ADDR` / `ADDR` with auto-dial) grows the mesh, and the outer
relay transport (multiplexing router, ephemeral handles, `Relay` addressing)
carries gossip for nodes with no direct path. A few tuning values (handle
rotation cadence, per-channel flow control) remain open and are noted below.
This document records decisions; the design discussion lives in the Obsidian
vault note `Design/Network Communication.md`.

## Design principles

1. A peer is a bidirectional byte stream. The protocol never refers to a peer by
   IP address or by a persistent node identifier.
2. All protocol logic (handshake, heartbeat, gossip) is transport-agnostic. A
   node cannot tell, and does not care, whether a peer stream is a direct TCP
   connection or a tunnel through a relay. The only relay-aware code is the step
   that establishes a stream.
3. All multi-byte integers are big-endian (network byte order), matching the
   transaction serialization.
4. Transport layering: the inner layer is this JeffyCoin protocol (`JEFY`
   frames). The outer layer is a relay transport that carries inner frames as an
   opaque blob. A relay never parses a `JEFY` frame.

## Layering overview

```
  node logic (handshake, heartbeat, gossip)   <- transport-agnostic
        |
    PeerLink  (a byte stream to one peer)       <- the seam
       /  \
 DirectLink  RelayLink
 (TcpStream) (one channel of the node's relay connection)
```

`PeerLink` is the abstraction that makes principle 2 hold. Establishing a
`DirectLink` versus a `RelayLink` differs; everything above `PeerLink` is identical.

## 1. Frame envelope

Every message is a fixed 16-byte header followed by a payload.

| Offset | Field | Size | Purpose |
|--------|-------|------|---------|
| 0 | `magic` | 4 | `JEFY` = `0x4A454659`; reject any frame that does not start with it |
| 4 | `version` | 2 (u16) | Wire-protocol version, starts at 1. Distinct from block/consensus version |
| 6 | `msg_type` | 2 (u16) | Message kind (see table in section 2) |
| 8 | `payload_length` | 4 (u32) | Byte length of the payload; reject if over the cap |
| 12 | `checksum` | 4 | First 4 bytes of `SHA256(payload)` |

Reader loop: read 16 bytes, validate `magic`, read `payload_length` bytes, verify
`checksum`, then dispatch on `msg_type`. A bad `magic` or `checksum` means the
stream is desynchronized or corrupt: drop the connection.

Payload cap: reject any frame whose `payload_length` exceeds `MAX_PAYLOAD`
(initial value to be set when blocks are added; gossip-v1 messages are under 200
bytes). The cap prevents a peer from forcing a huge allocation.

## 2. Message types (u16, grouped ranges)

| Value | Name | In gossip v1 |
|-------|------|--------------|
| `0x0001` | `HELLO` | yes |
| `0x0002` | `HELLO_ACK` | yes |
| `0x0003` | `PING` | yes |
| `0x0004` | `PONG` | yes |
| `0x0010` | `NEW_TRANSACTION` | yes |
| `0x0020` | `GET_ADDR` | yes |
| `0x0021` | `ADDR` | yes |
| `0x0030`+ | blocks and chain sync (`INV`, `GETDATA`, `NEW_BLOCK`, `GET_HEADERS`, `HEADERS`, `GET_BLOCKS`, `BLOCKS`) | reserved, later |

## 3. Handshake: `HELLO` / `HELLO_ACK` (symmetric)

No persistent node identifier appears in the protocol. `HELLO` payload:

| Field | Size | Purpose |
|-------|------|---------|
| `version` | 2 | Highest wire version the sender speaks |
| `nonce` | 8 | Random per-connection value; detects self-connection and duplicate links |
| `timestamp` | 8 | Unix seconds |
| `listen_port` | 2 | The sender's P2P listening port (the IP is read from the socket, when direct) |
| `services` | 8 | Capability bitfield, zero for now (reserved) |

Sequence: on a new connection, both sides send `HELLO` immediately, without
regard to who dialed. On receiving a `HELLO`:

1. If `version` is incompatible, disconnect.
2. If `nonce` equals the receiver's own outgoing `nonce`, the node connected to
   itself: disconnect.
3. Otherwise reply `HELLO_ACK`.

The link is established once each side has received a `HELLO_ACK`. Because the
handshake does not depend on who dialed, it works unchanged through a relay.

## 4. Heartbeat: `PING` / `PONG`

`PING` payload is an 8-byte nonce; `PONG` echoes it. Timers are named constants so
they are easy to change:

- `PING_INTERVAL` = 30 seconds of inactivity on a link before sending `PING`.
- `PEER_TIMEOUT` = 90 seconds with no message of any kind before dropping the peer.

Because a relay copies bytes transparently once paired, these heartbeats also keep
the relayed connection alive end to end; no separate relay keepalive is needed.

## 5. Transaction gossip: `NEW_TRANSACTION`

Payload is the exact 148-byte canonical transaction serialization
(`Transaction::serialize_to_bytes`). `payload_length` is 148.

Receive rule (reuses the node core and the decision in ADR 000):

1. Deserialize the 148 bytes; compute the transaction id.
2. If the id is already in the seen set, ignore it and do not relay (dedup).
3. Verify the signature. If invalid, ignore it (and count it toward a future
   blacklist).
4. If valid and new, add it to the seen set and send `NEW_TRANSACTION` to every
   connected peer except the one it arrived from.

Transactions are pushed in full. Blocks will instead use announce-then-request
(`INV` / `GETDATA`, and `GET_HEADERS` / `GET_BLOCKS` for chain sync), because
blocks are large and that path avoids sending full blocks to peers that already
have them. Transactions are 148 bytes, so pushing them is cheaper than a round
trip.

## 6. Peer-address exchange: `GET_ADDR` / `ADDR`

A node keeps a local database of known peers and shares it with any node that
asks (per the vault research note). `GET_ADDR` has an empty payload. `ADDR`
carries a list of peer address records.

A peer record lists one or more reachability endpoints, so it states how a peer
is reachable rather than assuming a dialable IP. An endpoint is one of:

- `Direct { ip, port }` — a directly dialable address (a public node).
- `Relay { relay_endpoint, handle }` — reach the peer through a relay, where
  `handle` is the peer's ephemeral directory handle (section 7). The handle lives
  only here and in the outer transport, and never appears in `HELLO` or in a
  transaction.

A node advertising itself bundles its own endpoints in one record, for example a
public node with relay fallback advertises both a `Direct` and a `Relay`
endpoint. `dial` tries `Direct` first and falls back to `Relay`, so a peer moves
between transports with no change above the `PeerLink` seam.

Stable cross-path node identity (recognizing that a `Direct` and a `Relay`
endpoint are the same node when they are not bundled in one record) is deferred.
Duplicate links are harmless because gossip dedups by transaction id. The exact
byte encoding of an endpoint and of the `ADDR` list is to be fixed when
`GET_ADDR` / `ADDR` are implemented.

## 7. Outer relay transport

Goal: two nodes behind carrier-grade NAT (CGNAT) talk through a relay that
forwards opaque bytes and understands no chain data, and the switch between relay
and direct links is seamless.

Settled model: a **multiplexing router**. Each node keeps one TCP connection to
the relay (both nodes dial out, which CGNAT permits). That connection carries
many **channels**; one channel is one logical peer link.

### Opacity

A channel carries an opaque byte stream, not framed `JEFY` messages. The relay
forwards byte chunks tagged by channel; it never learns where one `JEFY` message
ends and the next begins, so it never parses chain data. `RelayLink` presents a
channel to the node as a plain byte stream, and the inner `JEFY` reader frames
messages over it exactly as over a direct `TcpStream`. This is why the router
costs no seamlessness: `DirectLink` and `RelayLink` both implement `PeerLink`,
and node logic is identical.

### Outer frame (relay connection)

| Field | Size | Purpose |
|-------|------|---------|
| `channel_id` | 4 (u32) | Logical stream, local to this node-to-relay connection |
| `type` | 1 (u8) | Control or data kind (below) |
| `length` | 4 (u32) | Payload byte length |
| `payload` | varies | `DATA`: opaque bytes. `OPEN`: target handle. Control: small fields |

Channel ids are local to each side; the relay maps one node's channel id to the
paired peer's channel id. No global namespace and no persistent identifier.

Control and data types: `REGISTER`, `REGISTERED`, `OPEN`, `INCOMING`, `PAIRED`,
`DATA`, `CLOSE`, `ERROR`.

### Rendezvous: ephemeral directory handles

- A node that wants to be reachable sends `REGISTER`; the relay returns a short,
  random `handle` in `REGISTERED`. The relay keeps a directory of
  `handle -> that node's connection`.
- The node advertises `Relay { relay_endpoint, handle }` in its `ADDR` records,
  so peers learn the handle through gossip.
- To reach that peer, a node sends `OPEN` naming the handle. The relay finds the
  owner, sends it an `INCOMING` on a fresh channel, sends both sides `PAIRED`,
  and from then on routes `DATA` between the two channels. `CLOSE` tears a channel
  down; `ERROR` reports a missing handle or a failed pairing.
- The handle is ephemeral: a node gets a new one each time it connects to the
  relay (a new relay session), which keeps the map unpredictable across sessions
  while staying stable within a session. Rotation cadence must stay slow relative
  to `ADDR` gossip, or peers will hold stale handles; tune when implemented.

### Relationship to direct links and the transition to full peer-to-peer

Public nodes advertise a `Direct` endpoint; CGNAT nodes advertise a `Relay`
endpoint; a node with both advertises both. `dial` prefers `Direct` and falls
back to `Relay`. As nodes gain public addresses, their peers move from relay
channels to direct connections automatically, one peer at a time, with no change
above `PeerLink`. CGNAT friend nodes keep using the relay; "full peer-to-peer"
means the wider network of directly dialable public nodes, not CGNAT friends.
NAT hole punching to retire the relay entirely is a long-term stretch, out of
scope for now.

### Known limitations to revisit

- **Head-of-line blocking.** One TCP connection carrying all channels means a
  stalled reader on one channel backs up the others. Negligible for 148-byte
  transactions; revisit when large blocks arrive (per-channel flow-control
  windows, or a separate channel for bulk transfer).
- **Relay liveness.** The inner `PING`/`PONG` on each channel keeps the links and
  the relay connection busy. A relay-layer keepalive is needed only if every
  channel goes idle; decide at build time.

### Still to fix at implementation

Exact byte encodings of the control messages, handle length, handle-rotation
cadence, and whether per-channel flow control is added now or later.
