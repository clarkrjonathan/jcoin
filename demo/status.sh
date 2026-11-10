#!/bin/bash
# Height and tip of each node, and each key's balance (JEF) and nonce.
D="$(cd "$(dirname "$0")" && pwd)"
for p in 3000 3001 3002; do
  curl -s "127.0.0.1:$p/chain" | python3 -c "import sys,json; c=json.load(sys.stdin); print(':$p height', c['height'], 'tip', c['tip'][:12], 'target', c['target'][:10])"
done
while read -r n a; do
  curl -s "127.0.0.1:3000/balance/$a" | python3 -c "import sys,json; b=json.load(sys.stdin); print('$n', b['balance']/1e8, 'JEF  nonce', b['nonce'])"
done < "$D/keys.txt"
