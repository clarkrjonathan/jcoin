#!/bin/bash
# Start the demo network on the persisted chain (data-*/), plus the random
# transaction generator. Stop everything with stop.sh; the chain stays.
#
# Topology:
#   relay   127.0.0.1:9000  libp2p circuit relay + rendezvous
#   noah    REST :3000, P2P :8333, listens directly, uses the relay
#   jon     REST :3001, P2P :8334, listens directly, dials noah, uses the relay
#   holden  REST :3002, no listener (--no-listen), relay only: reachable only
#           through the relay, as a node behind carrier-grade NAT would be;
#           it finds the others through the relay's rendezvous service.
#   imac    the dedicated miner on the LAN (jcoin-miner.local, systemd unit
#           jcoin-node), listening on :8333. noah dials it; the laptop
#           firewall blocks inbound LAN connections, so the laptop dials out.
#           Its reward key "imac" is in keys/. Override with IMAC_PEER=.
set -e
D="$(cd "$(dirname "$0")" && pwd)"
BIN="$D/../jcoin/target/release"
W="$BIN/wallet --key-dir $D/keys"
NAMES=(noah jon holden)   # nodes started here
KEYS=(noah jon holden imac gpu)
# GPU=1 also starts the GPU miner (gpu-miner/, rewards to key "gpu") against
# noah's REST endpoint. Build it first: (cd ../gpu-miner && cargo build --release)
GPU_MINER="$D/../gpu-miner/target/release/gpu-miner"
IMAC_PEER="${IMAC_PEER:-10.0.0.88:8333}"
# REGTEST=1 runs every node on the regtest network (easy proof of work, blocks in
# milliseconds) for a fast local test. Default (unset) is MAINNET.
REGFLAG=""
[ "${REGTEST:-0}" = 1 ] && REGFLAG="--regtest"

for n in "${KEYS[@]}"; do
  [ -f "$D/keys/${n}_ed25519" ] || $W key new "$n" >/dev/null
done
: > "$D/keys.txt"
for n in "${KEYS[@]}"; do
  echo "$n $($W key show "$n" | awk '{print $3}')" >> "$D/keys.txt"
done
addr_of() { awk -v n="$1" '$1==n {print $2}' "$D/keys.txt"; }

: > "$D/pids"
# Relay. By default start a local relay on 127.0.0.1:9000. Set JCOIN_RELAY to a
# full relay multiaddr (for example the public AWS relay) to use that instead;
# then no local relay is started and every node dials the external one.
if [ -n "${JCOIN_RELAY:-}" ]; then
  RELAY="$JCOIN_RELAY"
  echo "using external relay: $RELAY"
else
  RELAY="$("$BIN/relay" --addr 127.0.0.1:9000 --key-file "$D/relay_key" --show-address)"
  setsid nohup "$BIN/relay" --addr 127.0.0.1:9000 --key-file "$D/relay_key" \
    > "$D/relay.log" 2>&1 < /dev/null &
  echo $! >> "$D/pids"
  sleep 1
fi

start_node() { # name rest-port args...
  local n=$1 port=$2; shift 2
  setsid nohup "$BIN/node" --rest-addr "127.0.0.1:$port" --data-dir "$D/data-$n" \
    --mine --miner "$(addr_of "$n")" --relay "$RELAY" $REGFLAG "$@" \
    > "$D/node-$n.log" 2>&1 < /dev/null &
  echo $! >> "$D/pids"
  sleep 1
}
start_node noah   3000 --p2p-addr 127.0.0.1:8333 --peer "$IMAC_PEER"
start_node jon    3001 --p2p-addr 127.0.0.1:8334 --peer 127.0.0.1:8333
start_node holden 3002 --no-listen

setsid nohup "$D/txgen.sh" > /dev/null 2>&1 < /dev/null &
echo $! >> "$D/pids"

if [ "${GPU:-0}" = 1 ]; then
  sleep 2
  setsid nohup "$GPU_MINER" --node http://127.0.0.1:3000 --miner "$(addr_of gpu)" \
    > "$D/gpu-miner.log" 2>&1 < /dev/null &
  echo $! >> "$D/pids"
fi
echo "relay: $RELAY"
echo "started: $(wc -l < "$D/pids") processes"
