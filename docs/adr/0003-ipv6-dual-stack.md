# ADR 0003 — IPv6 / dual-stack reachability

- Status: Proposed (2026-11-10)
- Related: `0002-deployment-model.md` (the relay and friend reachability),
  `../DEPLOY-RELAY.md`. Queued behind the v2 consensus restart (ADR 0001).

## Context

A live test on 2026-11-10 showed that hole punching (DCUtR) fails between a node
on Verizon cellular and a node behind a home router, and the AWS relay carried
the connection instead. The cause is **symmetric (carrier-grade) NAT on IPv4**:
the NAT assigns a different public port per destination, so the port the relay
observes is not the port a direct dial can use. Port prediction is impossible,
so the punch cannot land. T-Mobile Fiber (the maintainer's home ISP) is also
believed to use symmetric NAT on IPv4.

Both networks provide **IPv6**. IPv6 has no NAT: each device gets a globally
routable address with no port translation. This removes the symmetric-NAT
problem for any pair of peers that both have IPv6, and makes DCUtR reliable even
when a firewall blocks inbound (an IPv6 firewall pinhole keeps the same port, so
punching works where IPv4 symmetric NAT does not).

jcoin already has the building blocks: libp2p TCP supports `/ip6` multiaddrs, the
`identify` behaviour shares observed addresses, `dcutr` punches, and the node
enumerates interfaces over netlink. The nodes are simply **run IPv4-only** today
(`--p2p-addr 0.0.0.0:8333`, the relay address is `/ip4/...`).

## Decision

Make nodes dual-stack: listen on both IPv4 and IPv6, advertise the global IPv6,
and let peers connect directly over IPv6 when both ends have it. Keep the relay
as the fallback for IPv4-only peers. This is incremental and loses nothing:

- **Both peers have IPv6:** they connect directly over IPv6, bypassing CGNAT. No
  relay in the data path.
- **One peer is IPv4-only:** they use the relay, exactly as now.

Most networks today have IPv6 (all cellular, most large home ISPs), so dual-stack
is expected to move most traffic onto direct IPv6 links, with the relay covering
the IPv4-only minority.

### Changes

1. **Listen dual-stack.** In `p2p/host.rs`, call `listen_on` for both
   `/ip4/0.0.0.0/tcp/P` and `/ip6/::/tcp/P`, instead of a single address. The
   node binary (`bin/node.rs`) should accept this (a flag, or derive both from
   one port).
2. **Advertise the global IPv6.** Ensure the global (not link-local) IPv6 is the
   address added with `add_external_address` and shared through identify and
   rendezvous, so peers can dial it. Filter out link-local (`fe80::`) and
   unique-local addresses.
3. **Relay dual-stack (optional).** Have the relay also listen on `[::]:9000`, so
   IPv6-only friends can reach the relay over IPv6 as well.
4. **Firewall note.** If the home router firewalls inbound IPv6, either open the
   node's port for IPv6, or rely on DCUtR-over-IPv6 (reliable, same-port pinhole).

### Test

The same shape as the 2026-11-10 relay test, over IPv6: bring up a dual-stack
node, have a second IPv6 peer (the iMac on IPv6, or a friend) connect, and
confirm a **direct IPv6 connection** forms with no relay circuit in the path.

## Non-goals

- Dropping IPv4 or the relay. IPv4-only peers still need the relay.
- NAT64/464XLAT or any IPv4/IPv6 translation. An IPv6-only link to an IPv4-only
  peer is not expected to work directly; that pair uses the relay.

## Open questions

- Whether to expose dual-stack as one `--p2p-port` (bind both families) or keep
  explicit multiaddrs for each family.
- How aggressively to filter advertised addresses (drop link-local and
  unique-local; decide on private IPv4 in `add_external_address`).
