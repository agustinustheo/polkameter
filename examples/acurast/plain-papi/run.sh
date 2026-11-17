#!/bin/sh
# usage: test/run.sh <ws-endpoint> <output-dir>
# Runs setup (fund 4 derived accounts from //Alice), load (50 x Acurast.register at 0.25 tx/s), reconcile.
# Exit: 0 PASS, 1 FAIL, 4 CAN'T TELL, 3 setup failed before the load.
set -u
[ $# -eq 2 ] || { echo "usage: $0 <ws-endpoint> <output-dir>"; exit 3; }
DIR=$(cd "$(dirname "$0")" && pwd)
if [ ! -d "$DIR/node_modules/polkadot-api" ]; then
  (cd "$DIR" && npm install --no-audit --no-fund >/dev/null 2>&1) || { echo "npm install failed"; exit 3; }
fi
mkdir -p "$2" || exit 3
OUT=$(cd "$2" && pwd)
node "$DIR/stress.mjs" "$1" "$OUT"
