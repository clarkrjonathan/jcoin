//! The compute shader, in several variants that find the same nonces.
//!
//! - `Baseline`: `sha256.wgsl` as written by hand. A 64-word schedule array
//!   and a round loop that indexes a constant array.
//! - `Unrolled`: generated. Every schedule word and round is its own
//!   statement with the round constant as a literal, so the driver's compiler
//!   keeps everything in registers and folds the constant message words.
//!   The first hash word is compared before the other seven are computed.
//! - `Precomputed`: `Unrolled`, and it starts after round 5 from a state the
//!   CPU computed (`pow::prefix`), with schedule words 16 to 20 also from the
//!   CPU. Those do not depend on the nonce's low word, the only input that
//!   differs between the threads of one dispatch.

use crate::pow::K;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kernel {
    Baseline,
    Unrolled,
    Precomputed,
}

impl Kernel {
    pub const ALL: [Kernel; 3] = [Kernel::Baseline, Kernel::Unrolled, Kernel::Precomputed];

    pub fn name(self) -> &'static str {
        match self {
            Kernel::Baseline => "baseline",
            Kernel::Unrolled => "unrolled",
            Kernel::Precomputed => "precomputed",
        }
    }

    pub fn parse(name: &str) -> Result<Kernel, String> {
        Kernel::ALL
            .into_iter()
            .find(|k| k.name() == name)
            .ok_or(format!("unknown kernel '{name}' (baseline, unrolled, precomputed)"))
    }
}

/// Which shader to build and how many threads one workgroup has.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Config {
    pub kernel: Kernel,
    pub workgroup: u32,
}

impl Default for Config {
    fn default() -> Config {
        Config {
            kernel: Kernel::Baseline,
            workgroup: crate::gpu::WORKGROUP,
        }
    }
}

/// The WGSL source for `config`.
pub fn source(config: Config) -> String {
    match config.kernel {
        Kernel::Baseline => include_str!("sha256.wgsl").replace(
            "@workgroup_size(256)",
            &format!("@workgroup_size({})", config.workgroup),
        ),
        Kernel::Unrolled => generated(config.workgroup, 0),
        Kernel::Precomputed => generated(config.workgroup, 6),
    }
}

/// The unrolled kernel, starting at round `first` (0, or 6 with the CPU's
/// prefix). Matches `gpu::Params` field for field.
fn generated(workgroup: u32, first: usize) -> String {
    let mut s = String::from(
        "struct Params {
    midstate: array<u32, 8>,
    tail: array<u32, 5>,
    nonce_hi: u32,
    nonce_lo_base: u32,
    _pad: u32,
    goal: array<u32, 8>,
    pre: array<u32, 8>,
    wpre: array<u32, 5>,
}
struct Result {
    found: atomic<u32>,
    nonce_lo: atomic<u32>,
}
@group(0) @binding(0) var<storage, read> params: Params;
@group(0) @binding(1) var<storage, read_write> result: Result;
fn rotr(x: u32, n: u32) -> u32 {
    return (x >> n) | (x << (32u - n));
}
",
    );
    s += &format!("@compute @workgroup_size({workgroup})\n");
    s += "fn main(@builtin(global_invocation_id) gid: vec3<u32>) {\n";
    s += "    let lo = params.nonce_lo_base + gid.x;\n";

    // Message words 0..15: the header tail, the nonce, padding, the length.
    if first == 0 {
        for i in 0..5 {
            s += &format!("    let w{i} = params.tail[{i}];\n");
        }
    }
    s += "    let w5 = params.nonce_hi;\n    let w6 = lo;\n    let w7 = 0x80000000u;\n";
    for i in 8..15 {
        s += &format!("    let w{i} = 0u;\n");
    }
    s += "    let w15 = 1248u;\n"; // 156 bytes in bits
    // Schedule words 16..63, each from four earlier ones.
    for t in 16..64 {
        if first > 0 && t <= 20 {
            s += &format!("    let w{t} = params.wpre[{}];\n", t - 16);
            continue;
        }
        s += &format!(
            "    let w{t} = w{a} + (rotr(w{b}, 7u) ^ rotr(w{b}, 18u) ^ (w{b} >> 3u)) + w{c} + (rotr(w{d}, 17u) ^ rotr(w{d}, 19u) ^ (w{d} >> 10u));\n",
            a = t - 16,
            b = t - 15,
            c = t - 7,
            d = t - 2
        );
    }

    let start = if first == 0 { "midstate" } else { "pre" };
    for (i, v) in ["a", "b", "c", "d", "e", "f", "g", "h"].iter().enumerate() {
        s += &format!("    var {v} = params.{start}[{i}];\n");
    }
    s += "    var t1: u32;\n    var t2: u32;\n";
    for t in first..64 {
        s += &format!(
            "    t1 = h + (rotr(e, 6u) ^ rotr(e, 11u) ^ rotr(e, 25u)) + (g ^ (e & (f ^ g))) + {k:#010x}u + w{t};\n",
            k = K[t]
        );
        s += "    t2 = (rotr(a, 2u) ^ rotr(a, 13u) ^ rotr(a, 22u)) + ((a & b) | (c & (a | b)));\n";
        s += "    h = g; g = f; f = e; e = d + t1; d = c; c = b; b = a; a = t1 + t2;\n";
    }

    // hash < goal as big-endian 256-bit numbers. Nearly every thread stops at
    // the first word.
    s += "    let h0 = params.midstate[0] + a;\n";
    s += "    if (h0 > params.goal[0]) { return; }\n";
    s += "    var hash: array<u32, 8>;\n    hash[0] = h0;\n";
    for (i, v) in ["b", "c", "d", "e", "f", "g", "h"].iter().enumerate() {
        s += &format!("    hash[{n}] = params.midstate[{n}] + {v};\n", n = i + 1);
    }
    s += "    for (var i = 0u; i < 8u; i++) {
        if (hash[i] < params.goal[i]) {
            atomicMin(&result.nonce_lo, lo);
            atomicStore(&result.found, 1u);
            return;
        }
        if (hash[i] > params.goal[i]) {
            return;
        }
    }
}
";
    s
}
