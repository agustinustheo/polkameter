#!/bin/sh
# usage: test/run.sh <ws-endpoint> <output-dir>
# Polimec stress test: setup (fund 4 derived accounts from //Alice), load (50 x LinearRelease.vested_transfer
# at 0.25 tx/s for 200 s), reconcile against the finalized chain.
# Exit: 0 PASS, 1 FAIL, 3 setup failed before load, 4 CAN'T TELL.
[ $# -eq 2 ] || { echo "usage: $0 <ws-endpoint> <output-dir>"; exit 3; }
DIR=$(cd "$(dirname "$0")" && pwd)
if [ ! -d "$DIR/node_modules/polkadot-api" ]; then
  (cd "$DIR" && npm ci --no-audit --no-fund >/dev/null 2>&1) || { echo "SETUP FAILED: npm ci in $DIR"; exit 3; }
fi
mkdir -p "$2" || exit 3
exec node "$DIR/stress.mjs" "$1" "$2"
