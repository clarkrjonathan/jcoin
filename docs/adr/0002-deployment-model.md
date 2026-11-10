# ADR 0002 — Deployment model

- Status: Proposed (2026-11-10)
- Related: `0001-restart-consensus-v2.md` (the v2 restart this deployment model
  serves), `../DEPLOY-RELAY.md` (public relay on AWS, separate concern).

## Context

Deployment today is two halves that do not know about each other.

- **Laptop.** `demo/start.sh` is the control plane: it loads miner keys, starts
  a relay and three mining nodes from `jcoin/target/release`, each on its own
  `data-*/` dir, plus `txgen` and an optional GPU miner. Processes are `nohup`'d
  with PIDs in `demo/pids`; `stop.sh` kills them. It does not build; it assumes
  `target/release` is current.
- **iMac.** A systemd unit (`jcoin-node`, `Restart=always`, boot-enabled) runs a
  binary at `/usr/local/bin/jcoin-node`. `start.sh`/`stop.sh` do not touch it;
  noah dials it at a hardcoded `10.0.0.88:8333`.

Problems, ranked by how much they hurt a consensus restart (ADR 0001):

1. **No code provenance.** The iMac's `~/jcoin` is not a git repo; its binary has
   no recorded commit. The laptop runs the uncommitted working tree. A v2 restart
   needs every node to run byte-identical consensus code. A mismatch makes nodes
   silently reject each other's blocks, which looks exactly like "mining is
   slow." This is the central risk.
2. **iMac deploy is manual and out of band** (scp a binary, `ssh systemctl
   restart`), so it is easy to ship a mismatched binary (see 1).
3. **Nothing switches chains.** A v2 genesis is incompatible with the v1 data in
   every data dir; no script accounts for pointing nodes at a fresh chain.
4. **`start.sh` can run a stale binary** (no build step).
5. **No post-start verification** that nodes peered and agree on genesis.

Goals set for the new model:

- Reproducible: "all nodes run the same consensus code" is guaranteed, not hoped.
- Easy to build and run on both Linux and Windows.
- Deployable by non-technical friends on Windows, with a GPU, with no toolchain.
- Simple local bring-up and tear-down for development.

## Decision

Distribute prebuilt native binaries, built from one tagged commit per operating
system. Do not use Docker as the distribution or deployment mechanism. The GPU
miner already runs on any GPU through `wgpu` (Vulkan, DirectX 12, Metal), and the
node and miner are plain binaries, so native binaries serve both the maintainer's
machines and non-technical friends on Windows better than containers do.

Why not Docker: the GPU miner must run on bare metal for GPU access (container
GPU passthrough is finicky and platform-specific), so friends would still run the
miner natively. Docker would only wrap the node, while forcing a non-technical
Windows user to install Docker Desktop or WSL2 first. That is more friction, not
less. Docker's one real benefit, reproducibility, is obtained instead by building
every binary from one tagged commit and publishing checksums.

### 1. One source of truth: a tagged commit

Tag the release commit (for example `v2-genesis`). Build every binary for every
platform from that tag. Publish a checksum file. "All nodes run identical
consensus code" (problem 1) then holds because every binary came from the same
tagged source, verified by checksum. This replaces the current "the iMac runs
some build" with no provenance.

### 2. Build with CI per operating system

Build on GitHub Actions, one job per target: Linux x86-64 (maintainer's laptop
and iMac) and Windows x86-64 (friends). Native per-OS builds avoid cross-compile
trouble; `wgpu` has native graphics dependencies that are awkward to cross-build
from Linux, so a native Windows (MSVC) build is more reliable. CI also produces
the checksummed release artifacts automatically. This is a concrete reason to
push to GitHub (an open decision in ADR 0001). A local cross-build with
`x86_64-pc-windows-gnu` and mingw is a fallback for a quick one-off.

### 3. Friend bundle (Windows)

A zip holding:

- `jcoin-node.exe` — the node; connects to the network through the relay.
- `gpu-miner.exe` — mines on the friend's GPU through DirectX 12, no GPU runtime
  to install beyond the driver they already have.
- `start-mining.bat` — launches the node (relay address and the friend's key
  prefilled) and the miner pointed at the local node. Double-click to run.
- `README.txt` — including the Windows SmartScreen step (see friction below).

The friend unzips and double-clicks. No Docker, no WSL2, and no toolchain. The
friend also does not port-forward: libp2p traverses their NAT through the relay
(circuit relay v2 + rendezvous, with DCUtR upgrading to a direct connection when
possible). This assumes one publicly reachable relay exists; the demo relay runs
on localhost, so a public relay is a prerequisite for any friend to join (see
`../DEPLOY-RELAY.md`, an open item below).

### 4. Maintainer machines

- **Laptop.** `demo/start.sh` continues to run the local demo network from the
  built binaries; add a build step and a post-start genesis check (below).
- **iMac.** Keep the systemd unit, but deploy the binary from the tagged build
  (scp the release binary, verify its checksum, restart the unit), so it carries
  the same provenance as every other node.

### 5. Restart safeguards (carried from ADR 0001)

- Fresh data dirs for v2, keys preserved; leave the v1 dirs as the archive.
- A build step in `start.sh` so it never launches a stale binary.
- A post-start check that every node converges to the same tip as height climbs;
  divergence means a partition or a code mismatch.

## Friction to expect on Windows (friends)

- **SmartScreen / Defender.** An unsigned `.exe` that does networking and heavy
  GPU compute will trip "Windows protected your PC," and may be quarantined. Real
  code signing costs an annual fee and is not worth it here. Mitigate with a
  README step ("click More info, then Run anyway") and ship as a zip. This is the
  main barrier, more than the GPU.
- **Windows Firewall** prompts on first run; the friend clicks Allow.
- **Updates on a consensus change.** The height-activated versioning in ADR 0001
  (item 5) gives a grace window: a friend's old binary keeps working until the
  activation height, so they have time to download the new one.

## Costs of Docker (for the record)

Containerizing has no monetary cost for this project, but it does not fit the
friend path, which is why it is not used:

- Docker Engine and Podman are free and open source; near-zero overhead on Linux.
- Docker Desktop (the Windows/Mac GUI) is paid only for large commercial orgs;
  free for personal, educational, and small-business use. It can be avoided with
  WSL2 plus Docker Engine, or Podman.
- On Windows and Mac, containers run in a Linux VM: fine for a relay or CPU node,
  but a miner loses hash rate to the VM, and GPU passthrough is limited. Both
  reasons keep mining on bare metal regardless.

The existing `deploy/Dockerfile` for the relay stays as an optional convenience
for running a relay on a Linux host (see `../DEPLOY-RELAY.md`). Docker is not
required for any part of normal deployment.

## Non-goals

- Docker or any container runtime as a required part of deployment.
- Kubernetes or any orchestrator.
- Code signing the Windows binaries (revisit only if SmartScreen proves to be a
  real blocker for friends).
- Cloud hosting of nodes. The public relay on AWS is a separate concern
  (`../DEPLOY-RELAY.md`).

## Open questions

- CI now, or hand-built binaries until the first friend joins. CI needs the
  GitHub push decision from ADR 0001.
- Whether each friend runs their own node plus the miner (full participant), or a
  miner only against a shared node (simpler, but fewer real peers).
- Where the public relay runs (prerequisite for any friend to join): a cloud host
  (`../DEPLOY-RELAY.md`) or one port-forwarded machine. Until it exists, friends
  cannot traverse NAT regardless of the binaries.
