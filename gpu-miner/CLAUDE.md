# gpu-miner package — working notes

A library plus a program; usage is in `README.md` in this folder.

| File | Holds |
| --- | --- |
| `src/gpu.rs` | wgpu setup, `Gpu::open` (default kernel), `Gpu::open_with` (a `kernel::Config`), `Gpu::search` (one dispatch over a nonce range) |
| `src/kernel.rs` | The shader variants (`Kernel`: baseline, unrolled, precomputed), `Config` (kernel and workgroup size), `source` (the WGSL text; the last two are generated) |
| `src/sha256.wgsl` | The hand-written baseline kernel, the default for mining |
| `src/pow.rs` | Header midstate for the GPU, `prefix` (rounds 0 to 5 and schedule words 16 to 20, for the precomputed kernel), CPU check `meets_target` |
| `src/miner.rs` | `mine` (the loop against a node's REST endpoints), `MinerControl` (stop, GPU use limit, hash rate, found blocks, events), `rest_after` |
| `src/main.rs` | Command-line front end: `--list`, `--self-test`, `--bench`, `--bench-matrix`, `--kernel`, `--workgroup`, `--limit` |

- `gpu-miner --self-test [--kernel K]` compares GPU results with the CPU; run
  it for every kernel after any change to `gpu.rs`, `pow.rs`, or a kernel.
- `tools/gpu-bench` (`--bench-matrix`) checks and times every kernel, then
  workgroup and batch sizes for the fastest. Results so far, Vulkan on the
  laptop: NVIDIA RTX 2000 Ada 694 MH/s baseline, 1992 precomputed; Intel
  Iris Xe 157 and 555. Not measured: DirectX 12, AMD.
- The miner takes a new template every 20 s (`--refresh`), which is also how
  it picks up the stall fallback's easier target.
- `--bench N --limit P` measures the hash rate without a node.
