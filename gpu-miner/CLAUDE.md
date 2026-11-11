# gpu-miner package — working notes

A library plus a program; usage is in `README.md` in this folder.

| File | Holds |
| --- | --- |
| `src/gpu.rs` | wgpu setup, `Gpu::open`, `Gpu::search` (one dispatch over a nonce range) |
| `src/sha256.wgsl` | The SHA-256 kernel |
| `src/pow.rs` | Header midstate for the GPU, CPU check `meets_target` |
| `src/miner.rs` | `mine` (the loop against a node's REST endpoints), `MinerControl` (stop, GPU use limit, hash rate, found blocks, events), `rest_after` |
| `src/main.rs` | Command-line front end: `--list`, `--self-test`, `--bench`, `--limit` |

- `gpu-miner --self-test` compares GPU results with the CPU; run it after any
  change to `gpu.rs`, `pow.rs`, or the kernel.
- The miner takes a new template every 20 s (`--refresh`), which is also how
  it picks up the stall fallback's easier target.
- `--bench N --limit P` measures the hash rate without a node.
