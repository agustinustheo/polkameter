#!/bin/sh
# usage: test/run.sh <ws-endpoint> <output-dir>
# Myriad stress test: Tipping.send_tip at 0.25 tx/s for 200 s from 4 test accounts, via PolkaMeter calls mode.
# Exit 0 PASS, 1 FAIL (p95 >= 60 s or a proven loss), 4 CAN'T TELL, 3 setup failed.
# Extra category of PolkaMeter: "refused" (the node rejected it at submit) -> CAN'T TELL.
set -u
[ $# -eq 2 ] || { echo "usage: $0 <ws-endpoint> <output-dir>" >&2; exit 3; }
HERE=$(cd "$(dirname "$0")" && pwd)
PATH=${POLKAMETER_BIN:-$HOME/.local/bin}:$PATH
mkdir -p "$2" && OUT=$(cd "$2" && pwd) || exit 3
if [ ! -d "$HERE/node_modules/polkadot-api" ]; then
  (cd "$HERE" && npm install --no-audit --no-fund >/dev/null) || { echo "npm install failed" >&2; exit 3; }
fi
polkameter calls --chain "$1" --seed //Alice --accounts 4 --rate 0.25 --duration 200s \
  --calls "$HERE/tips.calls.ts" --max-p95 59999ms --output "$OUT"
code=$?
V=$(ls -t "$OUT"/*/calls-verdict.json 2>/dev/null | head -1)
if [ -n "$V" ]; then
  node -e 'const v=require(process.argv[1]); const d=require("path").dirname(process.argv[1]);
console.log(`\n== Myriad send_tip: ${v.verdict} (exit ${v.exitCode}) — ${v.reason}`);
console.log(`sent ${v.sent} (target ${v.target}, in window ${v.sentInWindow}), delivered ${v.deliveredRate} tx/s`);
console.log(`categories ${JSON.stringify(v.categories)}`);
console.log(`p95 submit->finalized ${v.p95FinalizedMs} ms (proven bounds ${v.p95AtLeastMs}..${v.p95AtMostMs})`);
console.log(`transactions: ${d}/transactions.jsonl`)' "$V"
fi
case $code in 0|1|3|4) exit $code ;; *) exit 3 ;; esac
