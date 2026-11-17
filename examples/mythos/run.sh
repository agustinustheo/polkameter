#!/bin/sh
# Mythos Nfts.mint stress test: 4 test accounts derived from Alith, 0.25 tx/s for 200 s (50 mints),
# each account minting into its own collection (created in setup). PolkaMeter calls mode does
# funding, finalized-block signing with mortality, nonce handling, resends, reconnects and reconciliation.
# usage: run.sh <ws-endpoint> <output-dir>
# exit: 0 PASS, 1 FAIL (p95 >= 60 s or a proven loss), 4 CAN'T TELL, 3 setup failed before the load.
# Categories (transactions.jsonl): finalized-ok, failed-in-block, lost, unresolved, plus PolkaMeter's
# `refused` (the node rejected it at submit; never in a block; gives CAN'T TELL, never FAIL).
set -u
[ $# -eq 2 ] || { echo "usage: $0 <ws-endpoint> <output-dir>"; exit 3; }
CHAIN=$1; OUT=$2
HERE=$(cd "$(dirname "$0")" && pwd)
PM_DIR=${POLKAMETER_DIR:-$HOME/.local/bin}
PATH="$PM_DIR:$PATH"; export PATH
SEED=${POLKAMETER_SEED:-0x5fb92d6e98884f76de468fa3f6278f8807c48bebc13595d45af5bdc4da702133}  # Alith

command -v polkameter >/dev/null || { echo "polkameter not found (set POLKAMETER_DIR)"; exit 3; }
if [ ! -d "$HERE/node_modules/polkadot-api" ]; then
  (cd "$HERE" && npm install --no-audit --no-fund) || { echo "npm install failed"; exit 3; }
fi
mkdir -p "$OUT" || exit 3

# --max-p95 59999ms: FAIL means p95 >= 60 s.
polkameter calls --chain "$CHAIN" --seed "$SEED" --accounts 4 --rate 0.25 --duration 200s \
  --calls "$HERE/mint.calls.ts" --max-p95 59999ms --output "$OUT"
code=$?

V=$(ls -t "$OUT"/*/calls-verdict.json 2>/dev/null | head -1)
if [ -n "$V" ]; then
  node -e '
    const v = JSON.parse(require("fs").readFileSync(process.argv[1], "utf8"));
    const s = (ms) => ms == null ? "n/a" : (ms / 1000).toFixed(1) + " s";
    console.log("\n== Mythos Nfts.mint stress result ==");
    console.log("run dir:", require("path").dirname(process.argv[1]));
    console.log("sent:", v.sent, " target:", v.target, " sent in window:", v.sentInWindow, " delivered rate:", v.deliveredRate, "tx/s");
    console.log("categories:", JSON.stringify(v.categories));
    console.log("p95 submit->finalized:", s(v.p95FinalizedMs), " (proven bounds: at least", s(v.p95AtLeastMs) + ", at most", s(v.p95AtMostMs) + ")");
    console.log("verdict:", v.verdict, "exit", v.exitCode, "-", v.reason);
  ' "$V"
fi
case $code in 0|1|3|4|130|143) exit $code ;; *) echo "polkameter exited $code"; exit 3 ;; esac
