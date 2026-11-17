#!/bin/sh
# usage: test/run.sh <ws-endpoint> <output-dir>
# Aventus TokenManager.transfer stress test. Exit: 0 PASS, 1 FAIL, 3 setup failed, 4 CAN'T TELL.
[ $# -eq 2 ] || { echo "usage: $0 <ws-endpoint> <output-dir>"; exit 3; }
DIR=$(cd "$(dirname "$0")" && pwd)
if [ ! -d "$DIR/node_modules/polkadot-api" ] || [ ! -d "$DIR/node_modules/@polkadot-api/descriptors" ]; then
  (cd "$DIR" && npm ci --no-audit --no-fund >/dev/null 2>&1) || { echo "npm ci failed in $DIR"; exit 3; }
fi
mkdir -p "$2" || exit 3
exec node "$DIR/stress.mjs" "$1" "$2"
