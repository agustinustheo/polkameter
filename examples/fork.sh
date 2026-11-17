#!/bin/sh
# Start a local Chopsticks copy of a live chain for the examples, and fund the seed account.
# Usage: examples/fork.sh <chain> <port>     Stop: kill the printed pid.
# Chopsticks builds a block only when a transaction arrives and finalizes it at once, so
# rates and latencies here are not those of the live network.
set -e
case $1 in
  acala) EP=wss://acala-rpc-0.aca-api.network ;;
  acurast) EP=wss://public-rpc.mainnet.acurast.com ;;
  aventus) EP=wss://avn-parachain.mainnet.aventus.io ;;
  darwinia) EP=wss://rpc.darwinia.network ;;
  ewx) EP=wss://public-rpc.mainnet.energywebx.com ;;
  mythos) EP=wss://mythos-rpc.dmarket.com/ ;;
  *) echo "usage: $0 <acala|acurast|aventus|darwinia|ewx|mythos> <port>" >&2; exit 2 ;;
esac
P=${2:?port}
npx --yes @acala-network/chopsticks@1.6.2 --endpoint "$EP" --port "$P" --rpc-timeout 180000 \
  --allow-unresolved-imports > "fork-$1.log" 2>&1 &
PID=$!
i=0
until curl -sf -H 'content-type: application/json' -d '{"id":1,"jsonrpc":"2.0","method":"system_chain"}' \
  "http://127.0.0.1:$P" > /dev/null; do
  i=$((i + 1)); [ $i -gt 150 ] && { echo "the fork did not start; see fork-$1.log" >&2; exit 1; }; sleep 2
done
# Fund the seed: Alith on the Ethereum-style chains, //Alice elsewhere.
case $1 in
  darwinia|mythos) SEED=0xf24FF3a9CF04c71Dbc94D0b566f7A27B94566cac ;;
  *) SEED=5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY ;;
esac
curl -sf -H 'content-type: application/json' "http://127.0.0.1:$P" -d "{\"id\":1,\"jsonrpc\":\"2.0\",\"method\":\"dev_setStorage\",\"params\":[{\"System\":{\"Account\":[[[\"$SEED\"],{\"providers\":1,\"data\":{\"free\":\"1000000000000000000000000000\"}}]]}}]}" > /dev/null
echo "$1 on ws://127.0.0.1:$P (pid $PID)"
