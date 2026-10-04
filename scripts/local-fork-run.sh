#!/usr/bin/env bash
# Runs an XML v2 plan through the Polkameter CLI on a fresh local Zombienet network.
#
# Usage: scripts/local-fork-run.sh NETWORK_TOML PLAN [OUTPUT_DIR]
#
# NETWORK_TOML is a Zombienet config, e.g. a Previewnet fork from `ppn fork toml <bundle> <out>`.
# Each run spawns it into a new directory, so a fork's snapshots give a fresh chain every time.
#
# Environment:
#   POLKAMETER_PLUGINS      plugin executables to install, separated by spaces
#   POLKAMETER_CREDENTIALS  credential profiles, `profile=ENV_VAR` separated by spaces
#   POLKAMETER              the CLI (default target/release/polkameter)
#   ZOMBIE_CLI, PPN_BIN_DIR zombie-cli and the collator binaries; both default to the directory
#                           of the toml's `default_command`
#
# The script waits until every WebSocket target of the plan produces and finalizes blocks,
# registers the plan's topology alias, runs the plan and always stops the network.
set -euo pipefail

toml=${1:?usage: $0 NETWORK_TOML PLAN [OUTPUT_DIR]}
plan=${2:?usage: $0 NETWORK_TOML PLAN [OUTPUT_DIR]}
out=${3:-target/local-runs/run-$(date +%s)}
cli=${POLKAMETER:-target/release/polkameter}
bin=$(dirname "$(sed -n 's/^default_command = "\(.*\)"/\1/p' "$toml" | head -1)")
zombie=${ZOMBIE_CLI:-$bin/zombie-cli}
[ -x "$zombie" ] || zombie=$(command -v zombie-cli)
# Previewnet's omni-node.sh wrapper finds the collator binary through PPN_BIN_DIR.
export PPN_BIN_DIR=${PPN_BIN_DIR:-$bin}

endpoints=$(grep -o 'endpoint="wss\{0,1\}://[^"]*"' "$plan" | sed 's/endpoint="ws\(s\{0,1\}\)\(:[^"]*\)"/http\1\2/' | sort -u)
topology=$(grep -o 'topology="[^"]*"' "$plan" | head -1 | sed 's/topology="\(.*\)"/\1/')
for url in $endpoints; do
	port=${url##*:}
	if (exec 3<>"/dev/tcp/127.0.0.1/${port%%/*}") 2>/dev/null; then
		echo "port ${port%%/*} is in use; stop the other network first" >&2
		exit 1
	fi
done

mkdir -p "$out"
out=$(cd "$out" && pwd)
network="$out/network"
export POLKAMETER_PLUGIN_REGISTRY="$out/plugins.json"

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

# Best and finalized height of the node at $1.
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
for url in $endpoints; do
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

for plugin in ${POLKAMETER_PLUGINS:-}; do
	"$cli" plugin install "$plugin" > /dev/null
done
for credential in ${POLKAMETER_CREDENTIALS:-}; do
	"$cli" plugin credential "${credential%%=*}" "${credential#*=}" > /dev/null
done
if [ -n "$topology" ]; then
	"$cli" plugin topology "$topology" "$network/zombie.json" > /dev/null
fi

status=0
"$cli" run "$plan" --output "$out/results" || status=$?
echo "polkameter run exited with $status; results in $out/results"
exit "$status"
