#!/bin/sh
# usage: test/run.sh <ws-endpoint> <output-dir>
# Darwinia stress test: setup (fund 4 derived accounts), load (Assets.transfer_keep_alive of asset 1026,
# 0.25 tx/s for 200 s), reconcile against the finalized chain.
# Exit: 0 PASS, 1 FAIL (chain missed a threshold), 3 setup failed before the load, 4 CAN'T TELL.
[ $# -eq 2 ] || { echo "usage: $0 <ws-endpoint> <output-dir>" >&2; exit 3; }
DIR=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$2" || exit 3
OUT=$(cd "$2" && pwd)
if [ ! -d "$DIR/node_modules/polkadot-api" ] || [ ! -d "$DIR/node_modules/@polkadot-api/descriptors" ]; then
  (cd "$DIR" && npm ci --no-audit --no-fund >"$OUT/npm-install.log" 2>&1) || { echo "npm ci failed; see $OUT/npm-install.log" >&2; exit 3; }
fi
exec node "$DIR/stress.mjs" "$1" "$OUT"
