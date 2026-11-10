// JeffyCoin proof of work on the GPU: find a nonce with SHA-256(header) < target.
//
// The header is 156 bytes, so SHA-256 processes three 64-byte chunks. The
// first two chunks (bytes 0..128) do not contain the nonce; the CPU compresses
// them once into a "midstate". Each invocation here compresses only the third
// chunk: header bytes 128..148 (the end of the target field), the 8-byte
// big-endian nonce, then SHA-256 padding for a 156-byte message.
//
// Each invocation tries one nonce: high word `nonce_hi`, low word
// `nonce_lo_base + global id`. The CPU keeps a batch inside one 2^32 range of
// low words, so the low word never wraps. A hit records the smallest
// qualifying low word with atomicMin, which makes the result deterministic.

struct Params {
    midstate: array<u32, 8>,
    tail: array<u32, 5>,      // header bytes 128..148, big-endian words
    nonce_hi: u32,
    nonce_lo_base: u32,
    _pad: u32,
    goal: array<u32, 8>,      // the target, big-endian words; hash must be strictly below
                              // ("target" is a reserved word in WGSL)
}

struct Result {
    found: atomic<u32>,
    nonce_lo: atomic<u32>,
}

@group(0) @binding(0) var<storage, read> params: Params;
@group(0) @binding(1) var<storage, read_write> result: Result;

var<private> K: array<u32, 64> = array<u32, 64>(
    0x428a2f98u, 0x71374491u, 0xb5c0fbcfu, 0xe9b5dba5u, 0x3956c25bu, 0x59f111f1u, 0x923f82a4u, 0xab1c5ed5u,
    0xd807aa98u, 0x12835b01u, 0x243185beu, 0x550c7dc3u, 0x72be5d74u, 0x80deb1feu, 0x9bdc06a7u, 0xc19bf174u,
    0xe49b69c1u, 0xefbe4786u, 0x0fc19dc6u, 0x240ca1ccu, 0x2de92c6fu, 0x4a7484aau, 0x5cb0a9dcu, 0x76f988dau,
    0x983e5152u, 0xa831c66du, 0xb00327c8u, 0xbf597fc7u, 0xc6e00bf3u, 0xd5a79147u, 0x06ca6351u, 0x14292967u,
    0x27b70a85u, 0x2e1b2138u, 0x4d2c6dfcu, 0x53380d13u, 0x650a7354u, 0x766a0abbu, 0x81c2c92eu, 0x92722c85u,
    0xa2bfe8a1u, 0xa81a664bu, 0xc24b8b70u, 0xc76c51a3u, 0xd192e819u, 0xd6990624u, 0xf40e3585u, 0x106aa070u,
    0x19a4c116u, 0x1e376c08u, 0x2748774cu, 0x34b0bcb5u, 0x391c0cb3u, 0x4ed8aa4au, 0x5b9cca4fu, 0x682e6ff3u,
    0x748f82eeu, 0x78a5636fu, 0x84c87814u, 0x8cc70208u, 0x90befffau, 0xa4506cebu, 0xbef9a3f7u, 0xc67178f2u,
);

fn rotr(x: u32, n: u32) -> u32 {
    return (x >> n) | (x << (32u - n));
}

@compute @workgroup_size(256)
fn main(@builtin(global_invocation_id) gid: vec3<u32>) {
    let lo = params.nonce_lo_base + gid.x;

    // Message schedule for the third chunk.
    var w: array<u32, 64>;
    for (var i = 0u; i < 5u; i++) {
        w[i] = params.tail[i];
    }
    w[5] = params.nonce_hi;
    w[6] = lo;
    w[7] = 0x80000000u;          // padding bit right after the 156 message bytes
    for (var i = 8u; i < 15u; i++) {
        w[i] = 0u;
    }
    w[15] = 156u * 8u;           // message length in bits
    for (var i = 16u; i < 64u; i++) {
        let s0 = rotr(w[i - 15u], 7u) ^ rotr(w[i - 15u], 18u) ^ (w[i - 15u] >> 3u);
        let s1 = rotr(w[i - 2u], 17u) ^ rotr(w[i - 2u], 19u) ^ (w[i - 2u] >> 10u);
        w[i] = w[i - 16u] + s0 + w[i - 7u] + s1;
    }

    var a = params.midstate[0];
    var b = params.midstate[1];
    var c = params.midstate[2];
    var d = params.midstate[3];
    var e = params.midstate[4];
    var f = params.midstate[5];
    var g = params.midstate[6];
    var h = params.midstate[7];
    for (var i = 0u; i < 64u; i++) {
        let s1 = rotr(e, 6u) ^ rotr(e, 11u) ^ rotr(e, 25u);
        let ch = (e & f) ^ (~e & g);
        let t1 = h + s1 + ch + K[i] + w[i];
        let s0 = rotr(a, 2u) ^ rotr(a, 13u) ^ rotr(a, 22u);
        let maj = (a & b) ^ (a & c) ^ (b & c);
        let t2 = s0 + maj;
        h = g;
        g = f;
        f = e;
        e = d + t1;
        d = c;
        c = b;
        b = a;
        a = t1 + t2;
    }

    var hash: array<u32, 8>;
    hash[0] = params.midstate[0] + a;
    hash[1] = params.midstate[1] + b;
    hash[2] = params.midstate[2] + c;
    hash[3] = params.midstate[3] + d;
    hash[4] = params.midstate[4] + e;
    hash[5] = params.midstate[5] + f;
    hash[6] = params.midstate[6] + g;
    hash[7] = params.midstate[7] + h;

    // hash < target, both as big-endian 256-bit numbers (word 0 most significant).
    for (var i = 0u; i < 8u; i++) {
        if (hash[i] < params.goal[i]) {
            atomicMin(&result.nonce_lo, lo);
            atomicStore(&result.found, 1u);
            return;
        }
        if (hash[i] > params.goal[i]) {
            return;
        }
    }
    // Equal to the target: not strictly below, so not a hit.
}
