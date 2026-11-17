#!/bin/sh
# usage: test/run.sh <ws-endpoint> <output-dir>
# Runs setup (fund 4 derived accounts from //Alice), load (50 Dex swaps at 0.25 tx/s) and reconciliation.
# Exit: 0 PASS, 1 FAIL, 4 CAN'T TELL, 3 setup failed before the load.
[ $# -eq 2 ] || { echo "usage: $0 <ws-endpoint> <output-dir>" >&2; exit 3; }
mkdir -p "$2" || exit 3
OUT=$(cd "$2" && pwd)
cd "$(dirname "$0")" || exit 3
if [ ! -d node_modules/polkadot-api ]; then
  npm ci --no-audit --no-fund >&2 || { echo "npm ci failed" >&2; exit 3; }
fi
exec node stress.mjs "$1" "$OUT"
