#!/bin/sh
# Polimec stress test: LinearRelease.vested_transfer at 0.25 tx/s for 200 s from 4 test accounts.
# usage: run.sh <ws-endpoint> <output-dir>   exit 0 PASS, 1 FAIL, 4 CAN'T TELL, 3 setup failed
# Needs Node.js >= 22.18 and `polkameter` (0.5.0-rc.1, plugin beside it) on PATH.
# The PolkaMeter run directory under <output-dir> holds transactions.jsonl and calls-verdict.json.
[ $# -eq 2 ] || { echo "usage: $0 <ws-endpoint> <output-dir>"; exit 3; }
cd "$(dirname "$0")" || exit 3
if [ ! -d node_modules/polkadot-api ] || [ ! -f .papi/descriptors/dist/index.js ]; then
  { npm install --no-audit --no-fund && npx papi generate; } >&2 || { echo "npm setup failed"; exit 3; }
fi
mkdir -p "$2" && OUT=$(cd "$2" && pwd) || exit 3
node keeper.mjs "$1" "$OUT/keeper.log" & K=$!     # Chopsticks only, see keeper.mjs
trap 'kill $K 2>/dev/null' EXIT INT TERM
polkameter calls --chain "$1" --seed //Alice --accounts 4 --rate 0.25 --duration 200s \
  --calls ./polimec.calls.ts --max-p95 59999ms --output "$OUT"
rc=$?
[ -f "$OUT/keeper.log" ] && sed 's/^/keeper: /' "$OUT/keeper.log"
exit $rc
