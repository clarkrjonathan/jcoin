#!/bin/bash
# Stop every process started by start.sh. Chain data stays in data-*/.
D="$(cd "$(dirname "$0")" && pwd)"
[ -f "$D/pids" ] && while read -r pid; do kill "$pid" 2>/dev/null; done < "$D/pids"
rm -f "$D/pids"; echo stopped
