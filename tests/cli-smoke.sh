#!/usr/bin/env bash
# Exercises the CLI end to end without a chain: install the example plugin, validate and run its
# plan locally and through a loopback remote agent, regenerate the report, and check that a
# failed assertion exits nonzero. Build first: cargo build -p polkameter --no-default-features --bin polkameter
# and cargo build -p polkameter-example-plugin.
set -euo pipefail

cli=${POLKAMETER:-target/debug/polkameter}
plugin=${POLKAMETER_EXAMPLE_PLUGIN:-target/debug/polkameter-example-plugin}
plan=examples/plugin-workflow.polkameter.xml
work=$(mktemp -d)
export POLKAMETER_PLUGIN_REGISTRY="$work/plugins.json"
agent=""
trap '[ -n "$agent" ] && kill "$agent" 2>/dev/null; rm -rf "$work"' EXIT

"$cli" plugin install "$plugin" > /dev/null
"$cli" validate "$plan" --format json | grep -q '"valid":true'
"$cli" preflight "$plan" --format json > /dev/null

"$cli" run "$plan" --output "$work/local" --format json > "$work/local.json"
run=$(ls -d "$work"/local/run-*)
"$cli" report "$run" --format json | grep -q '"exit_code":0'
for file in events.jsonl execution.json samples.jtl summary.md plots/throughput.svg; do
	[ -s "$run/$file" ] || { echo "missing $file" >&2; exit 1; }
done

sed 's/name="expected" value="84"/name="expected" value="85"/' "$plan" > "$work/failing.xml"
if "$cli" run "$work/failing.xml" --output "$work/failing" > /dev/null 2>&1; then
	echo "a failed assertion exited zero" >&2
	exit 1
fi

port=$((20000 + RANDOM % 20000))
export POLKAMETER_AGENT_TOKEN=smoke-token
"$cli" agent serve --bind "127.0.0.1:$port" --output-root "$work/remote" > "$work/agent.log" 2>&1 &
agent=$!
for _ in $(seq 1 50); do
	curl --silent --fail "http://127.0.0.1:$port/health" > /dev/null && break
	sleep 0.2
done
"$cli" run "$plan" --remote "http://127.0.0.1:$port" --remote-token-env POLKAMETER_AGENT_TOKEN \
	--format json | grep -q '"exit_code":0'
echo "CLI smoke passed"
