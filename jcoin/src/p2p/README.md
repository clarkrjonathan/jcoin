# JeffyCoin peer-to-peer protocol

Status: implemented and verified by unit and integration tests and by live
multi-node runs. Wire version 2. The JeffyCoin protocol (sections 1 to 6) runs
on libp2p (section 7): each peer session is one libp2p stream with the protocol
id `/jeffycoin/2.0.0`, over TCP or a relay circuit, encrypted with Noise and
multiplexed with Yamux. Nodes behind NAT reach each other through a circuit
relay, find each other with rendezvous, and upgrade to a direct connection
with hole punching (DCUtR) when the NATs allow it. Blocks are gossiped with
`NEW_BLOCK`, and a node catches up with `GET_BLOCKS` (a block locator) →
`BLOCKS`. This document records decisions; the design discussion lives in the
Obsidian vault note `Design/Network Communication.md`.

## Design principles

1. A JeffyCoin session is a bidirectional byte stream (a `PeerLink`). The
   protocol messages never carry an IP address as an identity.
2. All protocol logic (handshake, heartbeat, gossip, sync) is
   transport-agnostic. A session cannot tell whether its stream runs over a
   direct TCP connection or a relay circuit. Only the libp2p host knows.
3. Peer identity is a transport concern. libp2p gives each node an Ed25519 key;
   its peer id is the public key. The JeffyCoin messages do not repeat it.
4. All multi-byte integers are big-endian (network byte order), matching the
   transaction serialization.
5. A relay never parses a JeffyCoin frame. It carries encrypted bytes it cannot
   read.

## Layering overview

```
  session logic (handshake, heartbeat, gossip, sync)   <- transport-agnostic
        |
    PeerLink  (one libp2p stream, protocol /jeffycoin/2.0.0)   <- the seam
        |
    libp2p connection: Yamux over Noise over
       /                         \
   TCP (direct)            relay circuit (v2) through a relay node
                           (upgraded to direct by DCUtR when possible)
```

## 1. Frame envelope

Every message is a fixed 16-byte header followed by a payload.

| Offset | Field | Size | Purpose |
|--------|-------|------|---------|
| 0 | `magic` | 4 | `JEFY` = `0x4A454659`; reject any frame that does not start with it |
| 4 | `version` | 2 (u16) | Wire-protocol version, currently 2. Distinct from block/consensus version |
| 6 | `msg_type` | 2 (u16) | Message kind (see table in section 2) |
| 8 | `payload_length` | 4 (u32) | Byte length of the payload; reject if over `MAX_PAYLOAD` |
| 12 | `checksum` | 4 | First 4 bytes of `SHA256(payload)` |

Reader loop: read 16 bytes, validate `magic`, read `payload_length` bytes,
verify `checksum`, then dispatch on `msg_type`. A bad `magic` or `checksum`
means the stream is desynchronized or corrupt: drop the session. Noise already
protects integrity on the wire, so the checksum is a second check, kept so the
same framing works over any byte stream.

`MAX_PAYLOAD` is 4 MiB. It prevents a peer from forcing a huge allocation.

## 2. Message types (u16, grouped ranges)

| Value | Name |
|-------|------|
| `0x0001` | `HELLO` |
| `0x0002` | `HELLO_ACK` |
| `0x0003` | `PING` |
| `0x0004` | `PONG` |
| `0x0005` | `NODE_INFO` (friendly label, optional; added 2.2.2) |
| `0x0010` | `NEW_TRANSACTION` |
| `0x0020` | `GET_ADDR` |
| `0x0021` | `ADDR` |
| `0x0030` | `NEW_BLOCK` (gossip a block) |
| `0x0031` | `GET_BLOCKS` (sync request carrying a block locator) |
| `0x0032` | `BLOCKS` (a batch of blocks, ascending height) |

An unknown type is skipped; the session continues.

## 3. Handshake: `HELLO` / `HELLO_ACK` (symmetric)

`HELLO` payload:

| Field | Size | Purpose |
|-------|------|---------|
| `version` | 2 | Highest wire version the sender speaks |
| `nonce` | 8 | Random per-session value; detects a self-connection |
| `timestamp` | 8 | Unix seconds |
| `listen_port` | 2 | The sender's TCP listen port (informational) |
| `services` | 8 | Capability bitfield, zero for now (reserved) |

Sequence: when a session opens, both sides send `HELLO` immediately, without
regard to who opened the stream. On receiving a `HELLO`:

1. If `version` is below 2, close the session.
2. If `nonce` equals the receiver's own outgoing `nonce`, the node reached
   itself: close the session. (libp2p already refuses to dial its own peer id,
   so this is a second check.)
3. Otherwise reply `HELLO_ACK`.

The session is established once each side has received a `HELLO_ACK`.

### `NODE_INFO` (friendly label, optional)

After the handshake, a node that has a device label sends one `NODE_INFO`,
whose payload is the label as UTF-8 (cleaned to letters, digits, spaces, `-`,
`_`, at most 32 characters). It is cosmetic: the receiver shows it next to the
peer id in its connections list. It carries no identity and is not
authenticated, so the label is whatever the sender chose. It was added in
2.2.2; older nodes neither send it nor choke on it (an unknown type is
skipped), so new and old nodes still connect and sync.

## 4. Heartbeat: `PING` / `PONG`

`PING` payload is an 8-byte nonce; `PONG` echoes it.

- `PING_INTERVAL` = 30 seconds of inactivity on a session before sending `PING`.
- `PEER_TIMEOUT` = 90 seconds with no message of any kind before closing it.

libp2p's own ping protocol also runs on every connection.

## 5. Gossip and sync

**`NEW_TRANSACTION`.** The payload is the exact 153-byte canonical transaction
serialization (`Transaction::serialize_to_bytes`). Receive rule (ADR 000:
relay only fully valid transactions):

1. Decode the 153 bytes. An unknown transaction type does not decode.
2. Check the type (normal only) and the signature.
3. Offer it to the mempool, which checks the nonce and balance against the tip
   and the sender's pending chain (consecutive nonces, the whole chain
   affordable, no reuse of a pending nonce). One that skips ahead of the
   sender's chain is held (up to 16 per sender, for 10 minutes) and admitted
   when the gap fills.
4. If it is newly admitted, send it to every other session. If it was already
   pending, is held, or is rejected, do not relay it. Pending transfers are
   announced to every peer again each minute, so held ones that are later
   admitted still spread.

**`NEW_BLOCK`.** The payload is the block (156-byte header, `u16` transaction
count, 153 bytes per transaction). Blocks are pushed in full. A block is
relayed only after it is fully validated against its ancestry and stored. A
block whose parent is unknown triggers sync (`GET_BLOCKS`) from that peer.

**`GET_BLOCKS` / `BLOCKS`.** `GET_BLOCKS` carries a block locator: canonical
hashes from the tip back, dense near the tip and sparse toward genesis. The
peer answers with up to 500 canonical blocks after the most recent locator
hash it knows. A full batch that advanced the chain prompts the next request.

## 6. Peer-address exchange: `GET_ADDR` / `ADDR`

`GET_ADDR` has an empty payload. `ADDR` carries a list of libp2p multiaddrs,
each normally ending in `/p2p/<peer id>`:

- direct: `/ip4/203.0.113.5/tcp/8333/p2p/12D3Koo...`
- relayed: `/ip4/198.51.100.7/tcp/9000/p2p/<relay id>/p2p-circuit/p2p/<peer id>`

Encoding: a `u16` count, then for each address a `u16` length and the
multiaddr's binary encoding. An address longer than 512 bytes, invalid
multiaddr bytes, or trailing bytes reject the whole message.

A node sends its own addresses plus up to 64 from its address book. A receiver
reads at most 256 entries per message, keeps at most 1,000 addresses (a random
entry is evicted when full), and dials new ones within its peer cap. Dials
still in progress count toward the cap. Because the address names the peer id,
a dial reaches only that peer; a stale or forged address fails the Noise
handshake.

A peer reached only through the relay is the exception to "dial new ones
within the cap". Each direct address learned for it is dialed once, outside
the cap, because on the same LAN the dial usually works. When any direct
connection to a peer opens (from such a dial or from a hole punch), the host
closes the relayed connections to it. The session then reopens on the direct
connection. Since 2.2.3.

## 7. Transport: libp2p

### Stack

| Layer | Choice |
|-------|--------|
| Transport | TCP, and relay circuits (circuit relay v2) |
| Security | Noise (XX), authenticating the peer id |
| Multiplexing | Yamux |
| JeffyCoin sessions | one stream per peer, protocol id `/jeffycoin/2.0.0` |
| Liveness and info | libp2p ping, identify (protocol version `/jeffycoin/2.0.0`) |
| NAT traversal | DCUtR (direct connection upgrade through relay) |
| Discovery | rendezvous, namespace `jeffycoin`, served by the relay node |

### Identity

Each node has an Ed25519 key, stored in libp2p's protobuf encoding in
`<data-dir>/node_key` (or `--key-file`), mode 0600. Without a data dir the
identity is new on every start. The relay keeps its own key in `--key-file`
(default `relay_key`), so its peer id, and therefore the address nodes use for
it, survives restarts.

### Who opens the session

When a connection is established, the dialing side opens the
`/jeffycoin/2.0.0` stream, unless a session with that peer is already open or
opening. So a hole-punched direct connection that arrives after a relayed one
does not add a second session. If a session ends while the peer is still
connected (for example, its relayed connection closed after a hole punch), the
side with the smaller peer id reopens it on the remaining connection. Duplicate
sessions can still occur in rare races; they are harmless, because gossip
deduplicates.

### Relay and discovery flow (a node with `--relay`)

1. Dial the relay and listen on `<relay addr>/p2p-circuit` (a reservation).
2. When the relay accepts the reservation, the node's relayed address is
   `<relay addr>/p2p-circuit/p2p/<own id>`. The node adds it to its external
   addresses and to the addresses it advertises in `ADDR`.
3. Register that address with the relay's rendezvous service under `jeffycoin`
   (renewed every 30 minutes; the registration lasts 2 hours).
4. On every maintenance tick (10 seconds), discover registrations under
   `jeffycoin` and dial the new ones through their relayed addresses.
5. On a relayed connection, DCUtR tries to open a direct connection. If it
   succeeds, the session moves to it as described above. If it fails, the
   session stays on the relay.

A node that only dials out (behind carrier-grade NAT, `--no-listen`) still
connects directly to nodes that listen publicly: it learns their direct
addresses through `ADDR` or by hole punching, and dials them.

### Relay limits

libp2p's default relay limits (2 minutes and 128 KiB per circuit) fit only
hole-punch coordination. JeffyCoin also uses the relay as a lasting fallback,
so the relay node allows circuits of up to 12 hours and 1 GiB, 256 circuits in
total and 16 per peer. A circuit that reaches a limit closes; the nodes
rediscover each other and reconnect. The relay serves reservations only once it
knows a public address (`--external`, or a concrete `--addr`).

### What the relay can and cannot do

The relay forwards encrypted bytes. It cannot read the traffic and cannot
impersonate a node, because the Noise handshake runs end to end and
authenticates the peer id. It can refuse service, drop circuits, or lie in
rendezvous answers (for example, omit peers). A node that knows peers from
`ADDR` or `--peer` does not depend on it.

### Writing a node in another language

A compatible node needs:

1. A libp2p implementation with TCP, Noise, Yamux, identify, ping, and (for NAT
   traversal) the circuit relay v2 client, DCUtR, and the rendezvous client.
   go-libp2p, rust-libp2p, js-libp2p, nim-libp2p, and jvm-libp2p provide these.
2. The `/jeffycoin/2.0.0` stream: the frame envelope (section 1), the messages
   (sections 2 to 6), and the byte encodings of transactions and blocks
   (`src/lib.rs`, `src/block.rs`).
3. The consensus rules (`src/consensus.rs`, `src/block.rs`, `src/chain.rs`), so
   it accepts the same blocks.

### Known limitations

- The address book is not persisted; after a restart a node depends on
  `--peer` and the relay again.
- Only one relay per node (`--relay`). Several relays would remove the single
  point of failure for nodes behind NAT.
