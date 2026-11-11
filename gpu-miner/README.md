# GPU miner

`gpu-miner/` is a separate Cargo package. It mines JeffyCoin blocks on a GPU
and talks to a node only over REST, so the node has no GPU dependency.

## How it works

1. The miner asks a node for work: `GET /mining/work/{miner}`, where `miner`
   is the hex address that receives the reward. The node assembles a block
   (coinbase to `miner`, plus one applicable mempool transaction) and returns
   `job`, `height`, `header` (156 bytes, hex, nonce 0) and `target` (hex).
   The node keeps the last 64 templates.
2. The CPU computes the SHA-256 state after the first 128 header bytes (the
   midstate). Those bytes do not contain the nonce.
3. The GPU kernel (`src/sha256.wgsl`) compresses only the last chunk: header
   bytes 128..148, the 8-byte big-endian nonce, and the padding. Each thread
   tries one nonce. A hit records the smallest qualifying nonce in the batch
   with `atomicMin`, so the result is deterministic.
4. The CPU checks every hit with a full SHA-256 before submitting it:
   `POST /mining/submit {"job": <id>, "nonce": <u64>}`. The node puts the nonce
   into the stored template, checks the target, and submits the block to its
   core. `status` is `accepted`, `known`, `rejected`, `orphan`, `stale`, or
   `invalid` (HTTP 400).
5. Every 2 seconds the miner polls `GET /chain`. When the height reaches the
   job's height, another miner won it, and the miner fetches new work. It also
   fetches new work every `--refresh` seconds (default 20).

The kernel is WGSL run through wgpu 30, which uses Vulkan (Linux, Windows),
DirectX 12 (Windows), or Metal (macOS). It runs on NVIDIA, AMD, and Intel GPUs
without vendor toolkits. A hand-written CUDA kernel would likely be faster on
NVIDIA cards; portability was chosen instead.

## Use

```
cd gpu-miner
cargo build --release
./target/release/gpu-miner --list                 # GPUs, by index
./target/release/gpu-miner --self-test            # GPU results equal the CPU
./target/release/gpu-miner --bench 10             # hash rate
./target/release/gpu-miner --miner <hex address> --node http://127.0.0.1:3000
```

`--limit <percent>` (1 to 100) limits GPU use: the miner rests between GPU
dispatches so the GPU is busy for about that share of the time, and the hash
rate falls in proportion. It also applies to `--bench`.

`--adapter <index>` picks a GPU from `--list`. `--batch` sets nonces per
dispatch (default 2^22).

The node must be a build that has the `/mining/*` endpoints (added
2026-11-09). The node does not need `--mine`; the GPU miner does the mining.

## Measured

| GPU | Backend | Rate |
|---|---|---|
| Intel Iris Xe (i7-13700H laptop) | Vulkan | about 155-159 MH/s |

`--self-test` passed on that GPU (12 random headers, including ranges at the
top of the 32-bit low word, and a target no hash can meet).

## Known gaps

- One GPU per process. Run one process per GPU with `--adapter`.
- No optimizations beyond the midstate (for example, the first rounds of the
  last chunk that do not depend on the nonce are recomputed every time).
- The difficulty rule (12-block epochs, at most 4x per epoch) reacts slowly to
  a GPU joining or leaving. See `docs/HANDOFF.md`.
