# JeffyCoin peer-to-peer protocol

Status: gossip v1 is settled. The outer relay transport is a draft and is marked
as such. This document records decisions; the design discussion lives in the
Obsidian vault note `Design/Network Communication.md`.

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
 (TcpStream) (paired connection through a relay)
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

A peer address is a tagged union, so a record states how a peer is reachable
rather than assuming a dialable IP. This is what lets the relay be added later
without reshaping the record:

- `Direct { ip, port }` — used now, for direct connections.
- `Relay { relay_endpoint, routing_token }` — reserved; filled in with the relay
  transport. The `routing_token` is ephemeral and rotatable, lives only here and
  in the outer transport, and never appears in `HELLO` or in a transaction.

The exact byte encoding of the union and of the `ADDR` list is to be fixed when
`GET_ADDR` / `ADDR` are implemented.

## 7. Outer relay transport (DRAFT, not yet settled)

Goal: two nodes behind carrier-grade NAT (CGNAT) talk through a relay that
forwards bytes and understands no chain data, and the switch between relay and
direct links is seamless.

Direction under discussion:

- A relayed link uses one TCP connection to the relay per peer (no multiplexing).
  Both nodes dial out to the relay (CGNAT permits outbound). The relay pairs two
  connections, signals "paired," then copies bytes blindly. After pairing, a
  relayed connection is byte-identical to a direct connection, so the `JEFY`
  handshake and everything above it run unchanged.
- The only outer-protocol exchange is a short relay-control handshake at setup,
  before the `JEFY` `HELLO`, to request pairing under a token. The relay never
  sees a `JEFY` frame.
- Open: the rendezvous mechanism by which two outbound-only nodes agree on a
  pairing token without creating a persistent, predictable map. Candidates:
  out-of-band shared token per link; relay directory of ephemeral handles carried
  in `ADDR` `Relay` records. Must keep handles ephemeral and confined to the
  outer transport.

This section is updated once the outer transport is settled.
