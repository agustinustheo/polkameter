#!/bin/sh
# usage: run.sh <ws-endpoint> <output-dir>
# Darwinia stress test: Assets.transfer_keep_alive (asset 1026) at 0.25 tx/s for 200 s from 4 test accounts,
# with PolkaMeter calls mode. Exit 0 PASS, 1 FAIL, 4 CAN'T TELL, 3 setup failed before the load.
# Per-transaction lines: <output-dir>/<run>/transactions.jsonl; verdict: <output-dir>/<run>/calls-verdict.json.
# Categories: finalized-ok, failed-in-block, lost, unresolved, plus PolkaMeter's `refused` (rejected at submit,
# never in a block; gives CAN'T TELL).
[ $# -eq 2 ] || { echo "usage: $0 <ws-endpoint> <output-dir>" >&2; exit 3; }
CHAIN=$1; OUT=$2
HERE=$(cd "$(dirname "$0")" && pwd)
PM_DIR=${POLKAMETER_DIR:-$HOME/.local/bin}
PATH="$PM_DIR:$PATH"; export PATH
SEED=${POLKAMETER_SEED:-0x5fb92d6e98884f76de468fa3f6278f8807c48bebc13595d45af5bdc4da702133} # Alith

command -v polkameter >/dev/null || { echo "setup failed: polkameter not found in $PM_DIR" >&2; exit 3; }
if [ ! -d "$HERE/node_modules/polkadot-api" ] || [ ! -d "$HERE/node_modules/@polkadot-api/descriptors" ]; then
  (cd "$HERE" && npm install --no-audit --no-fund) || { echo "setup failed: npm install" >&2; exit 3; }
fi
mkdir -p "$OUT" || exit 3

polkameter calls --chain "$CHAIN" --seed "$SEED" --accounts 4 --rate 0.25 --duration 200s \
  --calls "$HERE/calls.ts" --max-p95 59999ms --output "$OUT"
code=$?

V=$(ls -t "$OUT"/*/calls-verdict.json 2>/dev/null | head -1)
if [ -n "$V" ]; then
  node -e '
    const v = JSON.parse(require("fs").readFileSync(process.argv[1], "utf8"));
    const ms = (x) => x == null ? "n/a" : (x / 1000).toFixed(1) + " s";
    console.log(`\n== Darwinia stress result (${process.argv[1]})`);
    console.log(`verdict ${v.verdict} (exit ${v.exitCode}): ${v.reason}`);
    console.log(`sent ${v.sent} of target ${v.target}, ${v.sentInWindow} in window, delivered ${v.deliveredRate} tx/s`);
    console.log(`categories ${JSON.stringify(v.categories)}`);
    console.log(`p95 submit->finalized ${ms(v.p95FinalizedMs)} (proven bounds ${ms(v.p95AtLeastMs)} .. ${ms(v.p95AtMostMs)})`);
  ' "$V"
fi
case $code in 0|1|3|4|130|143) exit $code ;; *) echo "polkameter exited $code" >&2; exit 3 ;; esac
