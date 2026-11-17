#!/bin/sh
# Acurast.register stress test: 0.25 tx/s for 200 s from 4 test accounts, PolkaMeter calls mode.
# usage: run.sh <ws-endpoint> <output-dir>
# Exit: 0 PASS, 1 FAIL (p95 >= 60 s or a proven loss), 4 CAN'T TELL, 3 setup failed, 2 bad usage.
set -u
[ $# -eq 2 ] || { echo "usage: $0 <ws-endpoint> <output-dir>" >&2; exit 2; }
HERE=$(cd "$(dirname "$0")" && pwd)
PM_BIN=${PM_BIN:-$HOME/.local/bin}
export PATH="$PM_BIN:$PATH"
mkdir -p "$2" && OUT=$(cd "$2" && pwd) || exit 3

if [ ! -d "$HERE/node_modules/polkadot-api" ]; then
  (cd "$HERE" && npm install --no-audit --no-fund >/dev/null 2>&1) || { echo "setup failed: npm install" >&2; exit 3; }
fi

polkameter calls --chain "$1" --seed //Alice --accounts 4 --rate 0.25 --duration 200s \
  --calls "$HERE/register.calls.ts" --max-p95 59999ms --output "$OUT"
rc=$?

V=$(ls -t "$OUT"/*/calls-verdict.json 2>/dev/null | head -1)
if [ -n "$V" ]; then
  echo "--- report ($(dirname "$V"))"
  node -e '
    const v = require(process.argv[1]);
    console.log("verdict:", v.verdict, "exit", v.exitCode, "-", v.reason);
    console.log("sent:", v.sent, "target:", v.target, "sentInWindow:", v.sentInWindow, "deliveredRate:", v.deliveredRate, "tx/s");
    console.log("categories:", JSON.stringify(v.categories));
    const s = (x) => x == null ? "n/a" : (x / 1000).toFixed(1) + " s";
    console.log("p95 submit->finalized:", s(v.p95FinalizedMs), "(proven bounds:", s(v.p95AtLeastMs), "..", s(v.p95AtMostMs) + ")");
    console.log("transactions:", require("path").join(require("path").dirname(process.argv[1]), "transactions.jsonl"));
  ' "$V"
fi
exit $rc
