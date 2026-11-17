#!/bin/sh
# usage: run.sh <ws-endpoint> <output-dir>
# Stress test of Acala Dex.swap_with_exact_supply (ACA -> AUSD, 1e15 planck) at 0.25 tx/s for 200 s from
# 4 test accounts derived from //Alice and funded by PolkaMeter calls mode.
# Exit: 0 PASS, 1 FAIL (p95 >= 60 s or a proven loss), 4 CAN'T TELL, 3 setup failed before the load.
# Categories in transactions.jsonl: finalized-ok, failed-in-block, lost, unresolved, plus PolkaMeter's
# "refused" (the node rejected it at submit, never in a block; it gives CAN'T TELL, never FAIL).
set -u
[ $# -eq 2 ] || { echo "usage: $0 <ws-endpoint> <output-dir>" >&2; exit 3; }
CHAIN=$1; OUT=$2
HERE=$(cd "$(dirname "$0")" && pwd)
PM_DIR=${POLKAMETER_DIR:-$HOME/.local/bin}
PATH="$PM_DIR:$PATH"; export PATH
mkdir -p "$OUT" || exit 3
OUT=$(cd "$OUT" && pwd)

if [ ! -d "$HERE/node_modules/polkadot-api" ]; then
  (cd "$HERE" && npm install --no-audit --no-fund --loglevel=error) || { echo "npm install failed" >&2; exit 3; }
fi

# p95 must be strictly below 60 s, so the limit is 59999 ms.
BEFORE=$(ls "$OUT")
{ polkameter calls --chain "$CHAIN" --seed //Alice --accounts 4 --rate 0.25 --duration 200s \
    --calls "$HERE/dex.calls.ts" --max-p95 59999ms --output "$OUT" 2>&1; echo $? > "$OUT/.exit"; } | tee "$OUT/polkameter.log"
CODE=$(cat "$OUT/.exit" 2>/dev/null); rm -f "$OUT/.exit"; [ -n "$CODE" ] || CODE=3
# This run's directory is the one that did not exist before; its calls-verdict.json decides.
V=
for d in $(ls "$OUT"); do
  echo "$BEFORE" | grep -qx "$d" && continue
  [ -f "$OUT/$d/calls-verdict.json" ] && V="$OUT/$d/calls-verdict.json"
done
if [ -n "$V" ]; then
  CODE=$(node -e 'const v=require(process.argv[1]);console.log(v.exitCode)' "$V")
  node -e '
    const v = require(process.argv[1])
    const c = v.categories, line = (k) => `${k}=${c[k] ?? 0}`
    console.log(`\n== ${v.verdict} (exit ${v.exitCode}): ${v.reason}`)
    console.log(`sent=${v.sent} target=${v.target} sentInWindow=${v.sentInWindow} deliveredRate=${v.deliveredRate} tx/s`)
    console.log(Object.keys(c).map(line).join(" "))
    console.log(`p95 submit->finalized: ${v.p95FinalizedMs} ms (proven bounds ${v.p95AtLeastMs}..${v.p95AtMostMs})`)
    console.log(`transactions: ${require("path").dirname(process.argv[1])}/transactions.jsonl`)
  ' "$V"
else
  # No verdict: the run ended before the load (setup failure) or on a signal.
  case $CODE in 0|1|4) CODE=3 ;; esac
fi
exit "$CODE"
