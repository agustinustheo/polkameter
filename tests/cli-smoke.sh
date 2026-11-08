#!/usr/bin/env bash
# Exercises the CLI end to end without a chain: install the example plugin, validate and run its
# plan, regenerate the report, and check that a failed assertion exits nonzero. Also replay a
# recorded load report and check the action's verdict extraction. Build first: cargo build -p polkameter --bin polkameter
# and cargo build -p polkameter-example-plugin.
set -euo pipefail

cli=${POLKAMETER:-target/debug/polkameter}
plugin=${POLKAMETER_EXAMPLE_PLUGIN:-target/debug/polkameter-example-plugin}
plan=examples/plugin-data-flow.polkameter.xml
work=$(mktemp -d)
export POLKAMETER_PLUGIN_REGISTRY="$work/plugins.json"
trap 'rm -rf "$work"' EXIT

"$cli" plugin install "$plugin" > /dev/null
"$cli" validate "$plan" --format json | grep -q '"valid":true'
"$cli" preflight "$plan" --format json > /dev/null

# Both verdict policies on a plan without a load test: there is no verdict to fail on.
"$cli" run "$plan" --output "$work/local" --junit "$work/reports/local.xml" --fail-on-verdict \
	--fail-on-inconclusive --format json > "$work/local.json"
run=$(ls -d "$work"/local/run-*)
[ ! -e "$run/summary.json" ]
"$cli" report "$run" --format json | grep -q '"exit_code":0'
cmp "$run/junit.xml" "$work/reports/local.xml"
xmllint --nonet --noout --schema tests/schemas/junit.xsd "$run/junit.xml"
for file in events.jsonl execution.json junit.xml samples.jtl summary.md plots/throughput.svg; do
	[ -s "$run/$file" ] || { echo "missing $file" >&2; exit 1; }
done

# Assertion failures exit 1 under the default policy; this plan has no load verdict.
sed 's/name="expected" value="84"/name="expected" value="85"/' "$plan" > "$work/failing.xml"
code=0
"$cli" run "$work/failing.xml" --output "$work/failing" > /dev/null 2> "$work/failing.err" || code=$?
[ "$code" -eq 1 ] || { echo "a failed assertion exited $code, expected 1" >&2; exit 1; }
xmllint --nonet --noout --schema tests/schemas/junit.xsd "$work"/failing/run-*/junit.xml
grep -q '^PolkaMeter: run failed: assertion verified failed: expected 85, got 84 (details in ' "$work/failing.err"

# A credential and an endpoint's keys reach no file of the run and no output of the CLI, though
# the plugin logs them, a failed assertion shows the endpoint and a failed step echoes the key.
cat > "$work/secret.xml" <<'EOF'
<polkameter-plan xmlns="https://polkameter.dev/schema/plan" version="1" name="Redaction" timeout-ms="30000">
  <plugins><plugin id="example" version="0.1.0" protocol="1"/></plugins>
  <credentials><credential id="key" profile="smoke"/></credentials>
  <targets><target id="chain" endpoint="wss://user:hunter2hunter2@127.0.0.1:1/ws?apikey=SmokeApiKey0123&amp;v=1"/></targets>
  <setup>
    <step id="logged" use="example.log-in-pieces"><input name="message" ref="credentials.key"/></step>
    <step id="endpoint" use="example.log-in-pieces"><input name="message" ref="targets.chain"/></step>
    <step id="compare" use="core.assert-equal"><input name="actual" ref="targets.chain"/><input name="expected" value="x"/></step>
  </setup>
  <teardown><step id="failure" use="example.fail-with-message"><input name="message" ref="credentials.key"/></step></teardown>
</polkameter-plan>
EOF
"$cli" plugin credential smoke POLKAMETER_SMOKE_SECRET > /dev/null
for format in human json; do
	out="$work/secret-$format"
	code=0
	POLKAMETER_SMOKE_SECRET='Smoke"Secret\Value' "$cli" run "$work/secret.xml" --output "$out" \
		--format "$format" > "$out.stdout" 2> "$out.stderr" || code=$?
	[ "$code" -eq 1 ] || { echo "the redaction run exited $code, expected 1" >&2; exit 1; }
	for needle in Secret hunter2 SmokeApiKey; do
		if grep -rlF -- "$needle" "$out" "$out.stdout" "$out.stderr"; then
			echo "$needle leaked into the files above" >&2
			exit 1
		fi
	done
	grep -qF 'before [redacted] after' "$out"/run-*/plugins/example/stderr.log
	grep -qF 'wss://[redacted]@127.0.0.1:1/ws?apikey=[redacted]&v=[redacted]' "$out.stderr"
done

# Exercise the action's jq expression on a real FAIL report, not the shell-test stub.
mkdir "$work/recorded"
cp crates/cli/tests/fixtures/smoke-run/*.json crates/cli/tests/fixtures/smoke-run/*.jsonl "$work/recorded/"
gzip -dc crates/cli/tests/fixtures/smoke-run/scrapes.jsonl.gz > "$work/recorded/scrapes.jsonl"
"$cli" report "$work/recorded" --format json > "$work/recorded.json" 2> "$work/report.log"
xmllint --nonet --noout --schema tests/schemas/junit.xsd "$work/recorded/junit.xml"
[ "$(jq -r '.verdict.result // empty' "$work/recorded/summary.json")" = FAIL ]

echo "CLI smoke passed"
