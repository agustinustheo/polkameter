#!/bin/sh
# usage: run.sh <ws-endpoint> <output-dir>
# Stress test: LiquidStaking.stake (150 EWT) at 0.25 tx/s for 200 s from 4 test accounts funded from //Alice.
# Exit: 0 PASS, 1 FAIL (p95 >= 60 s or a proven loss), 4 CAN'T TELL, 3 setup failed before the load.
[ $# -eq 2 ] || { echo "usage: $0 <ws-endpoint> <output-dir>" >&2; exit 3; }
DIR=$(cd "$(dirname "$0")" && pwd)
PM_DIR=${POLKAMETER_DIR:-$HOME/.local/bin}
PATH="$PM_DIR:$PATH"; export PATH
mkdir -p "$2" || exit 3
if [ ! -d "$DIR/node_modules/polkadot-api" ]; then
  (cd "$DIR" && npm install --silent --no-audit --no-fund) || { echo "npm install failed" >&2; exit 3; }
fi
# Each account sends 13 stakes of 150 EWT; top up to 5000 EWT (18 decimals) to cover them plus fees.
polkameter calls --chain "$1" --seed //Alice --accounts 4 --rate 0.25 --duration 200s \
  --calls "$DIR/stake.calls.ts" --fund 5000000000000000000000 --max-p95 59999ms --output "$2"
code=$?
case $code in 0|1|3|4) ;; 2) code=3 ;; esac
V=$(ls -td "$2"/run-*/ 2>/dev/null | head -1)
if [ -n "$V" ] && [ -f "$V/calls-verdict.json" ]; then
  node -e '
    const v = require(process.argv[1] + "/calls-verdict.json");
    console.log("--- result (" + process.argv[1] + ") ---");
    console.log("verdict:", v.verdict, "| reason:", v.reason);
    console.log("sent:", v.sent, "of target", v.target, "| sent in window:", v.sentInWindow, "| delivered rate:", v.deliveredRate, "tx/s");
    console.log("categories:", JSON.stringify(v.categories));
    console.log("p95 submit->finalized:", v.p95FinalizedMs, "ms (proven bounds", v.p95AtLeastMs, "..", v.p95AtMostMs, "ms)");
    console.log("per-transaction lines:", process.argv[1] + "transactions.jsonl");
  ' "$V"
fi
exit $code
