#!/usr/bin/env bash
# Runs a People XML plan through the Polkameter CLI on a fresh local Previewnet fork.
#
# Usage: scripts/local-people-run.sh NETWORK_TOML [PLAN] [OUTPUT_DIR]
#
# NETWORK_TOML is a Zombienet config from `ppn fork toml <bundle> <out>`. Each run spawns
# it into a new directory, so the fork snapshots give a fresh People chain every time.
# Build first: pnpm build && cargo build --release -p polkameter -p polkameter-scenarios.
# Binaries come from the directory of the toml's `default_command`; ZOMBIE_CLI and
# PPN_BIN_DIR override zombie-cli and the collator binaries.
set -euo pipefail

toml=${1:?usage: $0 NETWORK_TOML [PLAN] [OUTPUT_DIR]}
plan=${2:-examples/people-smoke.polkameter.xml}
out=${3:-target/local-people/run-$(date +%s)}
cli=${POLKAMETER:-target/release/polkameter}
plugin=${POLKAMETER_PEOPLE_PLUGIN:-target/release/polkameter-people-plugin}
bin=$(dirname "$(sed -n 's/^default_command = "\(.*\)"/\1/p' "$toml" | head -1)")
zombie=${ZOMBIE_CLI:-$bin/zombie-cli}
[ -x "$zombie" ] || zombie=$(command -v zombie-cli)
# Previewnet's omni-node.sh wrapper finds the collator binary through PPN_BIN_DIR.
export PPN_BIN_DIR=${PPN_BIN_DIR:-$bin}

for port in 10000 10010; do
	if (exec 3<>"/dev/tcp/127.0.0.1/$port") 2>/dev/null; then
		echo "port $port is in use; stop the other network first" >&2
		exit 1
	fi
done

mkdir -p "$out"
out=$(cd "$out" && pwd)
network="$out/network"
export POLKAMETER_PLUGIN_REGISTRY="$out/plugins.json"
export POLKAMETER_SETUP_SURI=${POLKAMETER_SETUP_SURI:-//Alice}

# Own process group, so the trap stops every node zombie-cli started.
set -m
"$zombie" spawn -p native -d "$network" "$toml" > "$out/network.log" 2>&1 &
group=$!
set +m
stop() {
	kill -TERM -- "-$group" 2>/dev/null || true
	pkill -TERM -f "$network" 2>/dev/null || true
	wait "$group" 2>/dev/null || true
}
trap stop EXIT

# Best and finalized must both advance on the relay and on People.
heights() {
	python3 - "$1" <<'EOF'
import json, sys, urllib.request
def rpc(method, params=()):
    body = json.dumps({"jsonrpc": "2.0", "id": 1, "method": method, "params": list(params)}).encode()
    request = urllib.request.Request(sys.argv[1], body, {"Content-Type": "application/json"})
    with urllib.request.urlopen(request, timeout=5) as response:
        return json.load(response)["result"]
best = int(rpc("chain_getHeader")["number"], 16)
finalized = int(rpc("chain_getHeader", [rpc("chain_getFinalizedHead")])["number"], 16)
print(best, finalized)
EOF
}
for url in http://127.0.0.1:10000 http://127.0.0.1:10010; do
	start="" deadline=$((SECONDS + 900))
	until now=$(heights "$url" 2>/dev/null) && [ -n "$start" ] &&
		[ "${now% *}" -gt "${start% *}" ] && [ "${now#* }" -gt "${start#* }" ]; do
		if ! kill -0 "$group" 2>/dev/null; then
			echo "the network exited; see $out/network.log" >&2
			exit 1
		fi
		if [ "$SECONDS" -gt "$deadline" ]; then
			echo "$url did not produce and finalize blocks in 15 minutes" >&2
			exit 1
		fi
		[ -z "$start" ] && start=${now:-}
		sleep 10
	done
	echo "$url is producing and finalizing (best/finalized $now)"
done

"$cli" plugin install "$plugin" > /dev/null
"$cli" plugin credential previewnet-sudo POLKAMETER_SETUP_SURI > /dev/null
"$cli" plugin topology previewnet "$network/zombie.json" > /dev/null

status=0
"$cli" run "$plan" --output "$out/results" || status=$?
echo "polkameter run exited with $status; results in $out/results"
exit "$status"
