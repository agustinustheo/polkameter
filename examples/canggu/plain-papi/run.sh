#!/bin/sh
# usage: test/run.sh <ws-endpoint> <output-dir>
# Exit: 0 PASS, 1 FAIL, 4 CAN'T TELL, 3 setup failed before the load. Tunables (env): RATE DURATION ACCOUNTS RECONCILE_MAX_S
[ $# -eq 2 ] || { echo "usage: $0 <ws-endpoint> <output-dir>" >&2; exit 3; }
DIR=$(cd "$(dirname "$0")" && pwd)
if [ ! -d "$DIR/node_modules/polkadot-api" ] || [ ! -d "$DIR/node_modules/@polkadot-api/descriptors" ]; then
  (cd "$DIR" && npm ci --no-audit --no-fund >/dev/null 2>&1) || { echo "npm ci failed (setup)" >&2; exit 3; }
fi
mkdir -p "$2" || exit 3
OUT=$(cd "$2" && pwd)
node "$DIR/stress.mjs" "$1" "$OUT"
