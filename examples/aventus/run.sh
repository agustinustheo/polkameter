#!/bin/sh
# usage: run.sh <ws-endpoint> <output-dir>
# Aventus TokenManager.transfer at 0.25 tx/s for 200 s from 4 test accounts derived from //Alice.
# --fund: AvN has ExistentialDeposit 0, so the default top-up is uncapped and fails (Arithmetic Underflow); 100 AVT each.
# Exit: 0 PASS, 1 FAIL, 4 CAN'T TELL, 3 setup failed before the load.
set -u
[ $# -eq 2 ] || { echo "usage: $0 <ws-endpoint> <output-dir>" >&2; exit 3; }
T=$(cd "$(dirname "$0")" && pwd)
PATH=$HOME/.local/bin:$PATH
mkdir -p "$2" || exit 3
[ -d "$T/node_modules/polkadot-api" ] || (cd "$T" && npm install --silent) || exit 3
polkameter calls --chain "$1" --seed //Alice --accounts 4 --rate 0.25 --duration 200s \
  --fund 100000000000000000000 --calls "$T/calls.ts" --max-p95 59999ms --output "$2"
c=$?
case $c in 0|1|3|4) exit $c ;; *) exit 3 ;; esac
