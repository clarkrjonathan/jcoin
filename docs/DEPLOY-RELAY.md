# Deploying the JeffyCoin relay (cost-minimized)

The relay (`src/bin/relay.rs` → `p2p::relay_server`) is a stateless, chain-blind
TCP router: it pairs node channels by ephemeral handle, serves a handle
directory so nodes find each other, and forwards opaque bytes. It needs one
public IP, one open TCP port, and almost no CPU or memory. These properties make
it cheap to run.

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

1. **Free tier (year 1): `t4g.micro` / `t3.micro`, 750 h/mo free for 12 months.**
   Effectively free if the account is new. Use this first.
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

## Deploy steps

1. Launch the smallest instance (Amazon Linux 2023 or Debian), ARM for
   `t4g`/Lightsail-arm.
2. **Firewall / security group:** allow inbound only on the relay port (default
   `9000/tcp`), and restrict the source to your friends' IPs if you can. Allow
   SSH only from your own IP. Nothing else inbound.
3. **Static IP:** attach a Lightsail static IP or an EC2 Elastic IP (free while
   attached). Share `RELAY_IP:9000` with the group.
4. **Install the binary and service:**
   ```
   scp target/.../release/relay  user@RELAY_IP:/tmp/jcoin-relay
   ssh user@RELAY_IP 'sudo install -m 0755 /tmp/jcoin-relay /usr/local/bin/jcoin-relay'
   scp deploy/jcoin-relay.service user@RELAY_IP:/tmp/
   ssh user@RELAY_IP 'sudo mv /tmp/jcoin-relay.service /etc/systemd/system/ && \
     sudo systemctl daemon-reload && sudo systemctl enable --now jcoin-relay'
   ```
   Logs: `journalctl -u jcoin-relay -f`.
5. **Point nodes at it:** start each CGNAT node with `--relay RELAY_IP:9000`.
   Thanks to the relay directory, nodes discover and open channels to each other
   automatically; no out-of-band handle sharing is needed.

## Cost-minimization checklist

- Smallest instance (nano/micro); ARM/Graviton over x86.
- Static musl binary → smallest OS image, no runtime packages to patch.
- Prefer Lightsail's bundled transfer to avoid metered egress entirely.
- Restrict the security group to the relay port (and known IPs) — also the main
  defense given the transport is unauthenticated (see below).
- A 1-year Savings Plan (~$2/mo) if you will keep it up; Spot for the absolute
  floor if interruptions are acceptable.
- The relay must stay up to be useful, so "stop when idle" does not apply; keep
  it on the cheapest always-on tier.

## Security notes (see `KNOWN-ISSUES.md`)

- The transport is **plaintext and unauthenticated** (V6). On a public IP the
  security group is your real access control — restrict the port to known IPs.
- The handle directory (`LIST`) returns every registered handle to any connector;
  acceptable inside a restricted security group, not for an open internet relay.
- Abuse limits are basic (a per-connection channel cap and one handle per
  connection). For a truly public relay add rate limiting and, ideally, TLS and
  peer authentication before exposing it widely.
