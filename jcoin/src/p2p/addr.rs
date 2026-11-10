//! Peer addresses and their wire encoding, used by `GET_ADDR` / `ADDR`.
//!
//! An address is a libp2p multiaddr. It says how to reach a peer and, with a
//! trailing `/p2p/<peer id>`, which peer to expect there. Examples:
//! - direct: `/ip4/203.0.113.5/tcp/8333/p2p/12D3Koo...`
//! - through a relay: `/ip4/198.51.100.7/tcp/9000/p2p/<relay id>/p2p-circuit/p2p/<peer id>`
//!
//! A node may be reachable several ways at once, so address exchange deals in
//! lists of addresses.

use libp2p::Multiaddr;

/// Longest encoded multiaddr accepted in an `ADDR` record. A relayed address
/// with two peer ids is about 120 bytes, so this only limits abuse.
pub const MAX_ADDR_LEN: usize = 512;

/// Encode an `ADDR` payload: a `u16` count, then for each address a `u16`
/// length and the multiaddr's binary encoding. Addresses longer than
/// `MAX_ADDR_LEN` are skipped, and at most `u16::MAX` are written.
pub fn encode_addrs(addrs: &[Multiaddr]) -> Vec<u8> {
    let encoded: Vec<Vec<u8>> = addrs
        .iter()
        .map(|a| a.to_vec())
        .filter(|bytes| bytes.len() <= MAX_ADDR_LEN)
        .take(u16::MAX as usize)
        .collect();
    let mut out = Vec::new();
    out.extend_from_slice(&(encoded.len() as u16).to_be_bytes());
    for bytes in encoded {
        out.extend_from_slice(&(bytes.len() as u16).to_be_bytes());
        out.extend_from_slice(&bytes);
    }
    out
}

/// Decode an `ADDR` payload. Returns `None` if the bytes are malformed, an
/// address is too long or not a valid multiaddr, or bytes trail the list.
pub fn decode_addrs(payload: &[u8]) -> Option<Vec<Multiaddr>> {
    let count = u16::from_be_bytes(payload.get(0..2)?.try_into().ok()?) as usize;
    let mut pos = 2;
    // Each record is at least its 2-byte length, so the payload bounds the
    // count: a lying count cannot force a large allocation.
    let mut addrs = Vec::with_capacity(count.min(payload.len() / 2));
    for _ in 0..count {
        let len = u16::from_be_bytes(payload.get(pos..pos + 2)?.try_into().ok()?) as usize;
        pos += 2;
        if len > MAX_ADDR_LEN {
            return None;
        }
        let bytes = payload.get(pos..pos + len)?;
        pos += len;
        addrs.push(Multiaddr::try_from(bytes.to_vec()).ok()?);
    }
    if pos != payload.len() {
        return None;
    }
    Some(addrs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Vec<Multiaddr> {
        vec![
            "/ip4/127.0.0.1/tcp/8333".parse().unwrap(),
            "/ip6/::1/tcp/8334".parse().unwrap(),
            "/ip4/203.0.113.5/tcp/9000/p2p/12D3KooWD3eckifWpRn9wQpMG9R9hX3sD158z7EqHWmweQAJU5SA/p2p-circuit/p2p/12D3KooWLTk7HhbbM8LqZ3ihp8xsWrQZbmRVsp2u3Vj1VXg5gWyg"
                .parse()
                .unwrap(),
        ]
    }

    #[test]
    fn addrs_round_trip() {
        let addrs = sample();
        assert_eq!(decode_addrs(&encode_addrs(&addrs)), Some(addrs));
    }

    #[test]
    fn empty_list_round_trips() {
        assert_eq!(decode_addrs(&encode_addrs(&[])), Some(Vec::new()));
    }

    #[test]
    fn truncated_or_trailing_payload_is_rejected() {
        let encoded = encode_addrs(&sample());
        assert!(decode_addrs(&encoded[..encoded.len() - 1]).is_none());
        let mut trailing = encoded.clone();
        trailing.push(0);
        assert!(decode_addrs(&trailing).is_none());
    }

    #[test]
    fn invalid_multiaddr_bytes_are_rejected() {
        // Count 1, length 3, then bytes that are not a multiaddr.
        let payload = [0, 1, 0, 3, 0xff, 0xff, 0xff];
        assert!(decode_addrs(&payload).is_none());
    }

    #[test]
    fn lying_count_does_not_allocate_or_decode() {
        assert!(decode_addrs(&[0xff, 0xff]).is_none());
    }
}
