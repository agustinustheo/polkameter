#!/usr/bin/env bash
# Exercises the CLI end to end without a chain: install the example plugin, validate and run its
# plan locally and through a loopback remote agent, regenerate the report, and check that a
# failed assertion exits nonzero. Also replay a recorded load report and check the action's
# verdict extraction. Build first: cargo build -p polkameter --bin polkameter
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

"$cli" run "$plan" --output "$work/local" --fail-on-verdict --format json > "$work/local.json"
run=$(ls -d "$work"/local/run-*)
[ ! -e "$run/summary.json" ]
"$cli" report "$run" --format json | grep -q '"exit_code":0'
for file in events.jsonl execution.json samples.jtl summary.md plots/throughput.svg; do
	[ -s "$run/$file" ] || { echo "missing $file" >&2; exit 1; }
done

# Assertion failures exit 1 under the default policy; this plan has no load verdict.
sed 's/name="expected" value="84"/name="expected" value="85"/' "$plan" > "$work/failing.xml"
code=0
"$cli" run "$work/failing.xml" --output "$work/failing" > /dev/null 2>&1 || code=$?
[ "$code" -eq 1 ] || { echo "a failed assertion exited $code, expected 1" >&2; exit 1; }

# Exercise the action's jq expression on a real FAIL report, not the shell-test stub.
mkdir "$work/recorded"
cp crates/cli/tests/fixtures/smoke-run/*.json crates/cli/tests/fixtures/smoke-run/*.jsonl "$work/recorded/"
gzip -dc crates/cli/tests/fixtures/smoke-run/scrapes.jsonl.gz > "$work/recorded/scrapes.jsonl"
"$cli" report "$work/recorded" --format json > "$work/recorded.json" 2> "$work/report.log"
[ "$(jq -r '.verdict.result // empty' "$work/recorded/summary.json")" = FAIL ]

port=$((20000 + RANDOM % 20000))
export POLKAMETER_AGENT_TOKEN=smoke-token
"$cli" agent serve --bind "127.0.0.1:$port" --output-root "$work/remote" > "$work/agent.log" 2>&1 &
agent=$!
for _ in $(seq 1 50); do
	curl --silent --fail "http://127.0.0.1:$port/health" > /dev/null && break
	sleep 0.2
done
"$cli" run "$plan" --remote "http://127.0.0.1:$port" --remote-token-env POLKAMETER_AGENT_TOKEN \
	--fail-on-verdict --format json | grep -q '"exit_code":0'
echo "CLI smoke passed"
