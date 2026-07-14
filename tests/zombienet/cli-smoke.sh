#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Starts a fresh native Zombienet relay, then proves every headless execution
# surface against it: validation, local preflight/run/report, and the remote
# agent preflight/run/report path. It never reuses a snapshot or a pre-existing
# node.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SCENARIO="$ROOT/tests/zombienet/cli-smoke.polkameter.json"
NETWORK_CONFIG="$ROOT/tests/zombienet/polkameter-relay.toml"
RPC_PORT=19144
PROMETHEUS_PORT=19161
AGENT_PORT=19901
RUST_VERSION="${RUST_VERSION:-1.93.0}"
OUTPUT_ROOT="${POLKAMETER_ZOMBIENET_OUTPUT_ROOT:-$ROOT/target/zombienet-cli-smoke}"
NETWORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/polkameter-zombienet.XXXXXX")"
LOG_DIR="$OUTPUT_ROOT/logs"
ZOMBIE_PID=""
AGENT_PID=""

find_binary() {
	local override="$1"
	shift
	if [ -n "$override" ]; then
		command -v "$override"
		return
	fi
	local candidate
	for candidate in "$@"; do
		if command -v "$candidate" >/dev/null 2>&1; then
			command -v "$candidate"
			return
		fi
	done
	echo "missing required executable: $1" >&2
	return 1
}

ZOMBIENET_BIN="$(find_binary "${POLKAMETER_ZOMBIENET_BIN:-}" zombienet zombie-cli)"
POLKADOT_BIN="$(find_binary "${POLKAMETER_POLKADOT_BIN:-}" polkadot)"
POLKADOT_BIN_DIR="$(cd "$(dirname "$POLKADOT_BIN")" && pwd)"
for worker in polkadot-prepare-worker polkadot-execute-worker; do
	if [ ! -x "$POLKADOT_BIN_DIR/$worker" ]; then
		echo "missing $worker beside $POLKADOT_BIN; run tests/zombienet/install-binaries.sh" >&2
		exit 1
	fi
done

cleanup() {
	local exit_code=$?
	trap - EXIT INT TERM
	if [ -n "$AGENT_PID" ] && kill -0 "$AGENT_PID" 2>/dev/null; then
		kill -TERM "$AGENT_PID" 2>/dev/null || true
		wait "$AGENT_PID" 2>/dev/null || true
	fi
	if [ -n "$ZOMBIE_PID" ] && kill -0 "$ZOMBIE_PID" 2>/dev/null; then
		kill -TERM "$ZOMBIE_PID" 2>/dev/null || true
		wait "$ZOMBIE_PID" 2>/dev/null || true
	fi
	rm -rf "$NETWORK_DIR"
	if [ "${POLKAMETER_ZOMBIENET_KEEP_ARTIFACTS:-0}" != "1" ]; then
		rm -rf "$OUTPUT_ROOT"
	fi
	exit "$exit_code"
}
trap cleanup EXIT INT TERM

require_free_port() {
	local port="$1"
	if lsof -nP -iTCP:"$port" -sTCP:LISTEN >/dev/null 2>&1; then
		echo "refusing to run: TCP port $port is already in use" >&2
		lsof -nP -iTCP:"$port" -sTCP:LISTEN >&2 || true
		exit 1
	fi
}

for port in "$RPC_PORT" "$PROMETHEUS_PORT" "$AGENT_PORT"; do
	require_free_port "$port"
done

rm -rf "$OUTPUT_ROOT"
mkdir -p "$LOG_DIR"

echo "Building the headless CLI"
cargo +"$RUST_VERSION" build --manifest-path "$ROOT/src-tauri/Cargo.toml" --bin polkameter
CLI="$ROOT/src-tauri/target/debug/polkameter"

echo "Spawning a fresh Zombienet relay on ws://127.0.0.1:$RPC_PORT"
if [ "$(basename "$ZOMBIENET_BIN")" = "zombie-cli" ]; then
	PATH="$POLKADOT_BIN_DIR:$PATH" \
		"$ZOMBIENET_BIN" spawn "$NETWORK_CONFIG" --provider native --dir "$NETWORK_DIR" \
		--node-verifier none >"$LOG_DIR/zombienet.log" 2>&1 &
else
	PATH="$POLKADOT_BIN_DIR:$PATH" \
		"$ZOMBIENET_BIN" --force --provider native --logType text --dir "$NETWORK_DIR" spawn "$NETWORK_CONFIG" \
		>"$LOG_DIR/zombienet.log" 2>&1 &
fi
ZOMBIE_PID=$!

wait_for_relay() {
	local attempt
	for attempt in $(seq 1 180); do
		if ! kill -0 "$ZOMBIE_PID" 2>/dev/null; then
			echo "Zombienet exited before the relay became ready:" >&2
			cat "$LOG_DIR/zombienet.log" >&2
			return 1
		fi
		if curl --silent --show-error --fail --max-time 2 \
			-H 'content-type: application/json' \
			--data '{"id":1,"jsonrpc":"2.0","method":"chain_getHeader","params":[]}' \
			"http://127.0.0.1:$RPC_PORT" >/dev/null 2>&1 \
			&& curl --silent --show-error --fail --max-time 2 \
			"http://127.0.0.1:$PROMETHEUS_PORT/metrics" >/dev/null 2>&1; then
			return 0
		fi
		sleep 1
	done
	echo "timed out waiting for the fresh Zombienet relay:" >&2
	tail -n 160 "$LOG_DIR/zombienet.log" >&2 || true
	return 1
}

wait_for_relay

echo "Validating the portable scenario"
"$CLI" validate "$SCENARIO" --format json >"$LOG_DIR/validate.json"

echo "Preflighting the fresh relay"
POLKAMETER_SURI='//Alice' \
	"$CLI" preflight "$SCENARIO" --signer-env POLKAMETER_SURI --format json \
	>"$LOG_DIR/local-preflight.json"

echo "Running and reporting locally"
LOCAL_OUTPUT="$OUTPUT_ROOT/local"
POLKAMETER_SURI='//Alice' \
	"$CLI" run "$SCENARIO" --signer-env POLKAMETER_SURI --output "$LOCAL_OUTPUT" --format json \
	>"$LOG_DIR/local-run.json"
LOCAL_ARTIFACT="$(find "$LOCAL_OUTPUT" -mindepth 1 -maxdepth 1 -type d -print -quit)"
test -n "$LOCAL_ARTIFACT"
"$CLI" report "$LOCAL_ARTIFACT" --format json >"$LOG_DIR/local-report.json"
rg -q ',true,' "$LOCAL_ARTIFACT/samples.jtl"
rg -q '| Failed | 0 |' "$LOCAL_ARTIFACT/summary.md"

echo "Starting the loopback remote agent"
POLKAMETER_AGENT_TOKEN='zombienet-cli-smoke-token' \
POLKAMETER_AGENT_SURI='//Alice' \
	"$CLI" agent serve --bind "127.0.0.1:$AGENT_PORT" --output-root "$OUTPUT_ROOT/remote" \
	>"$LOG_DIR/agent.log" 2>&1 &
AGENT_PID=$!

for attempt in $(seq 1 30); do
	if curl --silent --show-error --fail --max-time 2 "http://127.0.0.1:$AGENT_PORT/v1/health" \
		| rg -q '"status":"ready"'; then
		break
	fi
	if ! kill -0 "$AGENT_PID" 2>/dev/null; then
		echo "remote agent exited before becoming ready:" >&2
		cat "$LOG_DIR/agent.log" >&2
		exit 1
	fi
	sleep 1
done
curl --silent --show-error --fail --max-time 2 "http://127.0.0.1:$AGENT_PORT/v1/health" \
	| rg -q '"status":"ready"'

echo "Running and reporting through the remote agent"
POLKAMETER_REMOTE_TOKEN='zombienet-cli-smoke-token' \
	"$CLI" run "$SCENARIO" --remote "http://127.0.0.1:$AGENT_PORT" \
	--remote-token-env POLKAMETER_REMOTE_TOKEN --format json >"$LOG_DIR/remote-run.json"
REMOTE_ARTIFACT="$(find "$OUTPUT_ROOT/remote" -mindepth 1 -maxdepth 1 -type d -print -quit)"
test -n "$REMOTE_ARTIFACT"
"$CLI" report "$REMOTE_ARTIFACT" --format json >"$LOG_DIR/remote-report.json"
rg -q ',true,' "$REMOTE_ARTIFACT/samples.jtl"
rg -q '| Failed | 0 |' "$REMOTE_ARTIFACT/summary.md"

echo "Zombienet CLI smoke test passed. Artifacts: $OUTPUT_ROOT"
