# Deploying the JeffyCoin relay (cost-minimized)

> **Live deployment (2026-11-10).** A relay is running on AWS Lightsail (Ubuntu
> 24.04, flat 5 USD/mo, static IP `3.14.123.27`, instance `jeffy-relay`). It runs
> the glibc release binary under the `jcoin-relay` systemd unit (enabled,
> `Restart=always`, identity key in `/var/lib/jcoin-relay/relay_key`). Its address
> for `--relay` is:
>
> ```
> /ip4/3.14.123.27/tcp/9000/p2p/12D3KooWRGZbiiQPP4XRPttimhHQVGEAfkEokYoPRPE3g6feKL1v
> ```
>
> Point the demo at it with `JCOIN_RELAY=<that address> demo/start.sh`. One fix
> was needed during deploy: the hardened unit must allow `AF_NETLINK`, or the
> relay exits 1 at startup (libp2p reads network interfaces over netlink). The
> fix is in `deploy/jcoin-relay.service`.


The relay (`src/bin/relay.rs` → `p2p::relay_host`) is a chain-blind libp2p node:
a circuit relay (v2) that carries encrypted connections for nodes behind NAT,
plus a rendezvous point where those nodes register and find each other. Hole
punching (DCUtR) between the nodes moves their traffic off the relay when the
NATs allow it. It keeps only an identity key on disk. It needs one public IP,
one open TCP port, and almost no CPU or memory, so it is cheap to run.

## Build a lean static binary

The relay does not need the wallet's HTTP/TLS stack, so build it with the
`wallet` feature off. A static musl binary has no runtime dependencies and runs
on the smallest Linux image.

```
# from the cargo package dir (repo's jcoin/). Pick the target that matches the
# instance: x86_64 for t3/Lightsail-x86, aarch64 for t4g/Graviton/Lightsail-arm.
rustup target add x86_64-unknown-linux-musl        # or aarch64-unknown-linux-musl
cargo build --release --no-default-features --bin relay \
  --target x86_64-unknown-linux-musl
# => target/x86_64-unknown-linux-musl/release/relay   (self-contained, ~a few MB)
```

Build on your dev machine or CI, not on the nano instance — compiling the crate
needs more RAM than a 0.5 GB box has. Ship only the finished binary.

(Or use `deploy/Dockerfile`, which does the same static build in an Alpine image.)

## Cheapest AWS options, ranked

The workload is tiny: near-idle CPU, well under 128 MB RAM, and a few friends'
gossip is kilobytes per day, so data-transfer cost is effectively zero.

1. **Free tier: effectively free for the first several months on a new account.**
   AWS changed the free tier in mid-2025. A new account no longer gets the old
   "750 h/mo for 12 months"; it gets a credit-based plan (about 100 USD up front,
   more by completing setup tasks, over roughly a 6-month window). Either way a
   `t4g.micro` or `t3.micro` relay is covered for months. Verify the current
   terms when you sign up, since AWS keeps changing them.
2. **Lightsail, ~$3.50–5/mo (recommended for simplicity).** The smallest Linux
   bundle includes a **static IP and bundled data transfer**, so there are no
   surprise egress charges and pricing is flat. Easiest to reason about.
3. **EC2 `t4g.nano` on-demand, ~$3/mo (~$2 with a 1-year Savings Plan).** ARM
   Graviton is the cheapest general-purpose instance. An Elastic IP is free while
   attached. Egress is billed separately (~$0.09/GB after the monthly free tier),
   but at this traffic it rounds to $0.
4. **(Advanced) `t4g.nano` Spot, ~$1/mo.** Cheapest compute; AWS can reclaim it,
   but the relay is stateless and nodes auto-reconnect within ~5 s, so a hobby
   network tolerates interruptions.

Recommendation: free tier if eligible, otherwise Lightsail for predictable flat
cost, otherwise a `t4g.nano`.

## Non-AWS options (often cheaper or simpler)

AWS is not the cheapest host for one always-on box. The deploy steps below are
the same on any Linux VPS; only the console and signup differ.

1. **Oracle Cloud Always-Free, about 0 USD indefinitely.** Oracle's always-free
   tier includes ARM (Ampere) instances that are generous for this workload and
   do not expire. Caveats: signup can be fiddly, and Oracle sometimes reclaims
   always-free instances that look idle. Build an `aarch64` binary for it.
2. **Hetzner, about 4 EUR/mo.** The smallest Cloud VM is cheap, flat, and simple,
   with generous included traffic. Often the best price-to-simplicity for a
   single box.
3. **Vultr or DigitalOcean, about 2.50 to 6 USD/mo.** Flat pricing, easy console,
   static IP included. More expensive than Hetzner but widely documented.

The relay must stay up to be useful, so pick the cheapest always-on tier on any
of these, not a stop/start one.

## Deploy steps

1. Launch the smallest instance (Amazon Linux 2023 or Debian), ARM for
   `t4g`/Lightsail-arm.
2. **Firewall / security group:** allow inbound only on the relay port (default
   `9000/tcp`), and restrict the source to your friends' IPs if you can. Allow
   SSH only from your own IP. Nothing else inbound.
3. **Static IP:** attach a Lightsail static IP or an EC2 Elastic IP (free while
   attached). Nodes use the relay's full address, which you get in step 5.
4. **Install the binary and service:**
   ```
   scp target/.../release/relay  user@RELAY_IP:/tmp/jcoin-relay
   ssh user@RELAY_IP 'sudo install -m 0755 /tmp/jcoin-relay /usr/local/bin/jcoin-relay'
   scp deploy/jcoin-relay.service user@RELAY_IP:/tmp/
   ssh user@RELAY_IP 'sudo mv /tmp/jcoin-relay.service /etc/systemd/system/ && \
     sudo systemctl daemon-reload && sudo systemctl enable --now jcoin-relay'
   ```
   Logs: `journalctl -u jcoin-relay -f`.
   The unit passes `--external RELAY_IP:9000`: replace `RELAY_IP` with the
   static IP before installing it. The relay serves reservations only once it
   knows its public address.
5. **Get the relay's address:** the service creates its identity key in
   `/var/lib/jcoin-relay/relay_key` on first start and keeps it, so its peer id
   is stable. Read the full address from the log:
   ```
   ssh user@RELAY_IP 'journalctl -u jcoin-relay | grep "reachable at"'
   ```
   It prints `/ip4/RELAY_IP/tcp/9000/p2p/12D3Koo...`.
6. **Point nodes at it:** start each node with `--relay <that address>`. Nodes
   register with the relay's rendezvous service and discover each other; no
   out-of-band sharing is needed beyond the relay address.

## Cost-minimization checklist

- Smallest instance (nano/micro); ARM/Graviton over x86.
- Static musl binary → smallest OS image, no runtime packages to patch.
- Prefer Lightsail's bundled transfer to avoid metered egress entirely.
- Restrict the security group to the relay port (and, for a private network,
  known IPs).
- A 1-year Savings Plan (~$2/mo) if you will keep it up; Spot for the absolute
  floor if interruptions are acceptable.
- The relay must stay up to be useful, so "stop when idle" does not apply; keep
  it on the cheapest always-on tier.

## Security notes (see `KNOWN-ISSUES.md`)

- Every connection through the relay is encrypted end to end between the two
  nodes (Noise) and authenticated by peer id, so the relay cannot read the
  traffic or impersonate a node. It can refuse service, drop circuits, or omit
  peers from rendezvous answers.
- Rendezvous answers list every registered JeffyCoin node to any client that
  asks. That is the point for an open network; for a private one, restrict the
  security group to known IPs.
- libp2p's relay rate limits apply (reservations and circuits per peer and per
  IP), plus JeffyCoin's circuit caps (256 total, 16 per peer, 12 hours and
  1 GiB each; `relay_config` in `src/p2p/relay_host.rs`).
- Keep `relay_key` private: whoever holds it can run a relay with the same
  peer id.
