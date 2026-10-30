//! Peer addresses and their wire encoding, used by `GET_ADDR` / `ADDR` and by
//! the relay transport.
//!
//! An endpoint says how a peer is reachable. A node may be reachable several
//! ways at once (a public node with relay fallback), so address exchange deals
//! in endpoints, and `dial` prefers a `Direct` endpoint over a `Relay` one.

use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

/// Length of an ephemeral relay directory handle, in bytes.
pub const HANDLE_LEN: usize = 16;

/// An ephemeral relay directory handle. It is per relay session and appears
/// only in the outer relay transport and in `ADDR` records, never in `HELLO`
/// or a transaction.
pub type Handle = [u8; HANDLE_LEN];

/// One way to reach a peer.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Endpoint {
    /// A directly dialable TCP address (a public node).
    Direct(SocketAddr),
    /// Reach the peer through a relay by its directory handle.
    Relay { relay: SocketAddr, handle: Handle },
}

const TAG_DIRECT: u8 = 0x01;
const TAG_RELAY: u8 = 0x02;
const IP_V4: u8 = 0x04;
const IP_V6: u8 = 0x06;

fn encode_socket_addr(addr: &SocketAddr, out: &mut Vec<u8>) {
    match addr.ip() {
        IpAddr::V4(ip) => {
            out.push(IP_V4);
            out.extend_from_slice(&ip.octets());
        }
        IpAddr::V6(ip) => {
            out.push(IP_V6);
            out.extend_from_slice(&ip.octets());
        }
    }
    out.extend_from_slice(&addr.port().to_be_bytes());
}

fn decode_socket_addr(buf: &[u8], pos: &mut usize) -> Option<SocketAddr> {
    let family = *buf.get(*pos)?;
    *pos += 1;
    let ip: IpAddr = match family {
        IP_V4 => {
            let octets: [u8; 4] = buf.get(*pos..*pos + 4)?.try_into().ok()?;
            *pos += 4;
            IpAddr::V4(Ipv4Addr::from(octets))
        }
        IP_V6 => {
            let octets: [u8; 16] = buf.get(*pos..*pos + 16)?.try_into().ok()?;
            *pos += 16;
            IpAddr::V6(Ipv6Addr::from(octets))
        }
        _ => return None,
    };
    let port_bytes: [u8; 2] = buf.get(*pos..*pos + 2)?.try_into().ok()?;
    *pos += 2;
    Some(SocketAddr::new(ip, u16::from_be_bytes(port_bytes)))
}

impl Endpoint {
    fn encode(&self, out: &mut Vec<u8>) {
        match self {
            Endpoint::Direct(addr) => {
                out.push(TAG_DIRECT);
                encode_socket_addr(addr, out);
            }
            Endpoint::Relay { relay, handle } => {
                out.push(TAG_RELAY);
                encode_socket_addr(relay, out);
                out.extend_from_slice(handle);
            }
        }
    }

    fn decode(buf: &[u8], pos: &mut usize) -> Option<Endpoint> {
        let tag = *buf.get(*pos)?;
        *pos += 1;
        match tag {
            TAG_DIRECT => Some(Endpoint::Direct(decode_socket_addr(buf, pos)?)),
            TAG_RELAY => {
                let relay = decode_socket_addr(buf, pos)?;
                let handle: Handle = buf.get(*pos..*pos + HANDLE_LEN)?.try_into().ok()?;
                *pos += HANDLE_LEN;
                Some(Endpoint::Relay { relay, handle })
            }
            _ => None,
        }
    }
}

/// Encode an `ADDR` payload: a `u16` count followed by that many endpoints.
pub fn encode_addrs(endpoints: &[Endpoint]) -> Vec<u8> {
    let mut out = Vec::new();
    let count = endpoints.len().min(u16::MAX as usize) as u16;
    out.extend_from_slice(&count.to_be_bytes());
    for endpoint in endpoints.iter().take(count as usize) {
        endpoint.encode(&mut out);
    }
    out
}

/// Decode an `ADDR` payload. Returns `None` if the bytes are malformed.
pub fn decode_addrs(payload: &[u8]) -> Option<Vec<Endpoint>> {
    if payload.len() < 2 {
        return None;
    }
    let count = u16::from_be_bytes([payload[0], payload[1]]) as usize;
    let mut pos = 2;
    let mut endpoints = Vec::with_capacity(count);
    for _ in 0..count {
        endpoints.push(Endpoint::decode(payload, &mut pos)?);
    }
    Some(endpoints)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addrs_round_trip() {
        let endpoints = vec![
            Endpoint::Direct("127.0.0.1:8333".parse().unwrap()),
            Endpoint::Direct("[::1]:8334".parse().unwrap()),
            Endpoint::Relay {
                relay: "203.0.113.5:9000".parse().unwrap(),
                handle: [7u8; HANDLE_LEN],
            },
        ];
        let encoded = encode_addrs(&endpoints);
        let decoded = decode_addrs(&encoded).unwrap();
        assert_eq!(decoded, endpoints);
    }

    #[test]
    fn empty_list_round_trips() {
        let encoded = encode_addrs(&[]);
        assert_eq!(decode_addrs(&encoded), Some(Vec::new()));
    }

    #[test]
    fn truncated_payload_is_rejected() {
        let encoded = encode_addrs(&[Endpoint::Direct("127.0.0.1:1".parse().unwrap())]);
        assert!(decode_addrs(&encoded[..encoded.len() - 1]).is_none());
    }
}
