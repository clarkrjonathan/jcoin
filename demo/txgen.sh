#!/bin/bash
# Every 20-60 s, send a random amount (0.01-5.00 JEF, fee 0.01) from a random
# one of the three keys to another, through a random node. Logs to txgen.log.
D="$(cd "$(dirname "$0")" && pwd)"
BIN="$D/../jcoin/target/release"
NAMES=(noah jon holden)
while true; do
  sleep $((RANDOM % 41 + 20))
  s=$((RANDOM % 3)); r=$(((s + 1 + RANDOM % 2) % 3))
  sender=${NAMES[$s]}; recipient=${NAMES[$r]}
  to=$(awk -v n="$recipient" '$1==n {print $2}' "$D/keys.txt")
  cents=$((RANDOM % 500 + 1)); amount=$(printf "%d.%02d" $((cents / 100)) $((cents % 100)))
  port=$((3000 + RANDOM % 3))
  out=$("$BIN/wallet" --key-dir "$D/keys" --node "http://127.0.0.1:$port" \
        transaction new "$sender" "$to" "$amount" --fee 0.01 2>&1 | tr '\n' ' ')
  echo "$(date '+%F %T') $sender -> $recipient $amount JEF via :$port | $out" >> "$D/txgen.log"
done
