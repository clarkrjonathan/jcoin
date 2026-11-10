//! CPU side of the proof of work: the header layout, the SHA-256 midstate
//! that the GPU starts from, and a full CPU check of any nonce the GPU finds.

use sha2::{Digest, Sha256};

/// Serialized header length (see `jcoin::block::HEADER_LEN`).
pub const HEADER_LEN: usize = 156;
/// The nonce is the last 8 header bytes, big-endian.
pub const NONCE_OFFSET: usize = 148;

const H0: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

/// One SHA-256 compression of a 64-byte chunk into `state`.
fn compress(state: &mut [u32; 8], chunk: &[u8]) {
    let mut w = [0u32; 64];
    for i in 0..16 {
        w[i] = u32::from_be_bytes(chunk[4 * i..4 * i + 4].try_into().unwrap());
    }
    for i in 16..64 {
        let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
        let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
        w[i] = w[i - 16]
            .wrapping_add(s0)
            .wrapping_add(w[i - 7])
            .wrapping_add(s1);
    }
    let [mut a, mut b, mut c, mut d, mut e, mut f, mut g, mut h] = *state;
    for i in 0..64 {
        let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
        let ch = (e & f) ^ (!e & g);
        let t1 = h
            .wrapping_add(s1)
            .wrapping_add(ch)
            .wrapping_add(K[i])
            .wrapping_add(w[i]);
        let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
        let maj = (a & b) ^ (a & c) ^ (b & c);
        let t2 = s0.wrapping_add(maj);
        h = g;
        g = f;
        f = e;
        e = d.wrapping_add(t1);
        d = c;
        c = b;
        b = a;
        a = t1.wrapping_add(t2);
    }
    for (s, v) in state.iter_mut().zip([a, b, c, d, e, f, g, h]) {
        *s = s.wrapping_add(v);
    }
}

/// The work the GPU needs for one header: the SHA-256 state after the first
/// 128 bytes, header bytes 128..148 as big-endian words, and the target as
/// big-endian words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GpuWork {
    pub midstate: [u32; 8],
    pub tail: [u32; 5],
    pub target: [u32; 8],
}

pub fn gpu_work(header: &[u8; HEADER_LEN], target: &[u8; 32]) -> GpuWork {
    let mut midstate = H0;
    compress(&mut midstate, &header[0..64]);
    compress(&mut midstate, &header[64..128]);
    let word = |b: &[u8], i: usize| u32::from_be_bytes(b[4 * i..4 * i + 4].try_into().unwrap());
    GpuWork {
        midstate,
        tail: std::array::from_fn(|i| word(&header[128..148], i)),
        target: std::array::from_fn(|i| word(target, i)),
    }
}

/// SHA-256 of the header with `nonce` in place (the consensus block hash).
pub fn block_hash(header: &[u8; HEADER_LEN], nonce: u64) -> [u8; 32] {
    let mut h = *header;
    h[NONCE_OFFSET..].copy_from_slice(&nonce.to_be_bytes());
    Sha256::digest(h).into()
}

/// True if the header with `nonce` meets the target (hash strictly below).
pub fn meets_target(header: &[u8; HEADER_LEN], nonce: u64, target: &[u8; 32]) -> bool {
    block_hash(header, nonce) < *target
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The midstate path (two chunks on the CPU, the padded third chunk
    /// assembled exactly as the shader does) equals a plain SHA-256.
    #[test]
    fn midstate_plus_last_chunk_equals_sha256() {
        for seed in 0u8..20 {
            let mut header = [0u8; HEADER_LEN];
            for (i, b) in header.iter_mut().enumerate() {
                *b = (i as u8).wrapping_mul(31).wrapping_add(seed.wrapping_mul(7));
            }
            let nonce = 0x0123_4567_89ab_cdefu64.wrapping_mul(seed as u64 + 1);
            let work = gpu_work(&header, &[0xff; 32]);

            let mut chunk = [0u8; 64];
            for i in 0..5 {
                chunk[4 * i..4 * i + 4].copy_from_slice(&work.tail[i].to_be_bytes());
            }
            chunk[20..28].copy_from_slice(&nonce.to_be_bytes());
            chunk[28] = 0x80;
            chunk[56..64].copy_from_slice(&(HEADER_LEN as u64 * 8).to_be_bytes());
            let mut state = work.midstate;
            compress(&mut state, &chunk);
            let mut got = [0u8; 32];
            for i in 0..8 {
                got[4 * i..4 * i + 4].copy_from_slice(&state[i].to_be_bytes());
            }
            assert_eq!(got, block_hash(&header, nonce), "seed {seed}");
        }
    }
}
