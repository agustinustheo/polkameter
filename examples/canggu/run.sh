#!/bin/sh
# usage: run.sh <ws-endpoint> <output-dir>
# Payroll.batch_pay at 5 tx/s for 120 s from 8 test accounts derived from //Alice, via PolkaMeter calls mode.
# Exit: 0 PASS, 1 FAIL (p95 >= 30 s or a proven loss), 4 CAN'T TELL, 3 setup failed.
CHAIN=$1; OUT=$2
[ -n "$CHAIN" ] && [ -n "$OUT" ] || { echo "usage: run.sh <ws-endpoint> <output-dir>"; exit 3; }
HERE=$(cd "$(dirname "$0")" && pwd)
PATH=$HOME/.local/bin:$PATH
mkdir -p "$OUT" && OUT=$(cd "$OUT" && pwd) || exit 3
if [ ! -d "$HERE/node_modules/polkadot-api" ]; then
  (cd "$HERE" && npm ci --no-audit --no-fund) || { echo "setup failed: npm ci"; exit 3; }
fi
cd "$HERE" || exit 3
polkameter calls --chain "$CHAIN" --seed //Alice --accounts 8 --rate 5 --duration 120s \
  --calls ./payroll.calls.ts --max-p95 29999ms --output "$OUT"
code=$?
V=$(ls -t "$OUT"/*/calls-verdict.json 2>/dev/null | head -1)
if [ -n "$V" ]; then
  echo "--- $(dirname "$V")"
  node -e 'const v=require(process.argv[1]); const c=v.categories;
console.log(`verdict ${v.verdict} (exit ${v.exitCode}): ${v.reason}`);
console.log(`sent ${v.sent} of target ${v.target}; in window ${v.sentInWindow}; delivered ${v.deliveredRate} tx/s`);
console.log("categories", JSON.stringify(c), "(refused = rejected at submit, never in a block)");
console.log(`p95 submit->finalized ${v.p95FinalizedMs} ms (proven bounds ${v.p95AtLeastMs}..${v.p95AtMostMs})`);
console.log("per-transaction lines:", require("path").dirname(process.argv[1])+"/transactions.jsonl");' "$V"
fi
exit $code
