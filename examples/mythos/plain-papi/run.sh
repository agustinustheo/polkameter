#!/bin/sh
# usage: run.sh <ws-endpoint> <output-dir>   -> exit 0 PASS, 1 FAIL, 3 setup failed, 4 CAN'T TELL
[ $# -eq 2 ] || { echo "usage: $0 <ws-endpoint> <output-dir>"; exit 3; }
mkdir -p "$2" && OUT=$(cd "$2" && pwd) || { echo "cannot create $2"; exit 3; }
cd "$(dirname "$0")" || exit 3
if [ ! -d node_modules/polkadot-api ]; then
  npm ci --no-audit --no-fund >/dev/null 2>&1 || npm install --no-audit --no-fund >/dev/null 2>&1 || { echo "npm install failed"; exit 3; }
fi
exec node stress.mjs "$1" "$OUT"
