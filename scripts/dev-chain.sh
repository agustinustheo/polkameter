#!/usr/bin/env bash
# A local Polkadot SDK dev chain for load tests: polkadot-omni-node in dev mode running the
# parachain template runtime, which has the standard System, Balances, Timestamp and Sudo
# pallets and funded dev accounts (//Alice ... //Ferdie). One process, no relay chain; every
# block is final as soon as it is produced.
#
#   scripts/dev-chain.sh start    download (once), start, and wait until the chain is ready
#   scripts/dev-chain.sh stop     stop the node this script started
#   scripts/dev-chain.sh status   print whether it runs, and its best block
#
# Downloads are pinned and checked against SHA-256 sums. Supported hosts: Linux x86_64 and
# macOS arm64. Needs bash and curl, plus sha256sum or shasum.
#
# Variables:
#   POLKAMETER_DEV_CHAIN_DIR   where binaries, the chain spec, data and the log go
#                              (default: target/dev-chain under the current directory)
#   POLKAMETER_DEV_CHAIN_PORT  JSON-RPC port, ws:// and http:// (default: 9944)
#   POLKAMETER_DEV_BLOCK_MS    block time in milliseconds (default: 6000, the runtime's slot)
#   POLKAMETER_DEV_WAIT_BLOCKS blocks to wait for before start returns (default: 21; PolkaMeter's
#                              preflight measures the block time over 20 blocks)
#   POLKAMETER_DEV_POOL_TYPE   transaction pool: single-state (default) or fork-aware. In dev
#                              mode, this release's fork-aware pool accepts transactions that no
#                              block then includes: examples/substrate-remarks.polkameter.xml
#                              finalized 2,826 of 62,993 with it, and all with single-state.
#   POLKAMETER_DEV_RUNTIME     your own runtime's .wasm instead of the template runtime. It must
#                              run on polkadot-omni-node (a parachain runtime with Aura) and have
#                              a genesis preset named by POLKAMETER_DEV_PRESET (default:
#                              development). Its block time is the runtime's slot duration.
#   POLKAMETER_DEV_NODE_ARGS   extra polkadot-omni-node arguments, split on spaces
set -euo pipefail

SDK_RELEASE=polkadot-stable2609-1
TEMPLATE_RELEASE=v0.0.5
RUNTIME_SHA256=fe7031fc468153199f2eea940bd87737988d013b05e4bbdabeac690281f76fd7
OMNI_NODE_SHA256_LINUX_X86_64=a30cc63c7794eec8c22d92bbb103c6757b8dcab6c6f550eb480bec0edd5716cb
OMNI_NODE_SHA256_MACOS_ARM64=2cd1ddb414bc6bba46be19052ff8f4b95172761ff8b465d8c5a506e9c9b4f0ce

dir=${POLKAMETER_DEV_CHAIN_DIR:-target/dev-chain}
port=${POLKAMETER_DEV_CHAIN_PORT:-9944}
block_ms=${POLKAMETER_DEV_BLOCK_MS:-6000}
wait_blocks=${POLKAMETER_DEV_WAIT_BLOCKS:-21}
pool_type=${POLKAMETER_DEV_POOL_TYPE:-single-state}
custom_runtime=${POLKAMETER_DEV_RUNTIME:-}
preset=${POLKAMETER_DEV_PRESET:-development}
read -r -a extra_args <<< "${POLKAMETER_DEV_NODE_ARGS:-}"

node="$dir/bin/polkadot-omni-node"
template_runtime="$dir/bin/parachain_template_runtime.compact.compressed.wasm"
runtime=${custom_runtime:-$template_runtime}
spec="$dir/chain-spec.json"
pidfile="$dir/node.pid"
log="$dir/node.log"

die() { echo "dev-chain: $*" >&2; exit 1; }

sha256() {
	if command -v sha256sum > /dev/null; then sha256sum "$1" | cut -d' ' -f1
	else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

# fetch URL FILE SHA256: download unless FILE already has the sum, then check it.
fetch() {
	local url=$1 file=$2 sum=$3
	if [ -f "$file" ] && [ "$(sha256 "$file")" = "$sum" ]; then return; fi
	echo "dev-chain: downloading $url" >&2
	curl -fsSL --retry 5 --retry-all-errors --connect-timeout 30 -o "$file.part" "$url"
	local got
	got=$(sha256 "$file.part")
	if [ "$got" != "$sum" ]; then
		rm -f "$file.part"
		die "checksum mismatch for $url: expected $sum, got $got"
	fi
	mv "$file.part" "$file"
}

install() {
	local suffix sum
	case "$(uname -s)-$(uname -m)" in
		Linux-x86_64) suffix="" sum=$OMNI_NODE_SHA256_LINUX_X86_64 ;;
		Darwin-arm64) suffix="-aarch64-apple-darwin" sum=$OMNI_NODE_SHA256_MACOS_ARM64 ;;
		*) die "no pinned polkadot-omni-node for $(uname -s) $(uname -m); use Linux x86_64 or macOS arm64" ;;
	esac
	mkdir -p "$dir/bin"
	fetch "https://github.com/paritytech/polkadot-sdk/releases/download/$SDK_RELEASE/polkadot-omni-node$suffix" "$node" "$sum"
	chmod +x "$node"
	if [ -z "$custom_runtime" ]; then
		fetch "https://github.com/paritytech/polkadot-sdk-parachain-template/releases/download/$TEMPLATE_RELEASE/parachain_template_runtime.compact.compressed.wasm" "$template_runtime" "$RUNTIME_SHA256"
	fi
	[ -f "$runtime" ] || die "no runtime at $runtime"
	# The spec is rebuilt on every start, so a rebuilt runtime is always the one that runs.
	"$node" chain-spec-builder -c "$spec" create --relay-chain rococo-local --para-id 1000 \
		--runtime "$runtime" named-preset "$preset" > "$dir/chain-spec.log" 2>&1 \
		|| { cat "$dir/chain-spec.log" >&2; die "could not build a chain spec from $runtime with preset $preset"; }
}

running() {
	[ -f "$pidfile" ] && kill -0 "$(cat "$pidfile")" 2> /dev/null
}

rpc() {
	curl -fsS --max-time 5 -H 'Content-Type: application/json' \
		-d "{\"id\":1,\"jsonrpc\":\"2.0\",\"method\":\"$1\",\"params\":[]}" "http://127.0.0.1:$port"
}

best_block() {
	local number
	number=$(rpc chain_getHeader 2> /dev/null | sed -n 's/.*"number":"\(0x[0-9a-fA-F]*\)".*/\1/p')
	[ -n "$number" ] && printf '%d\n' "$number"
}

start() {
	running && die "already running (pid $(cat "$pidfile")); run: $0 stop"
	if rpc system_health > /dev/null 2>&1; then die "port $port is in use"; fi
	install
	rm -rf "$dir/data"
	nohup "$node" --chain "$spec" --dev --dev-block-time "$block_ms" --base-path "$dir/data" \
		--rpc-port "$port" --no-prometheus --no-telemetry --pool-type "$pool_type" ${extra_args[@]+"${extra_args[@]}"} > "$log" 2>&1 &
	echo $! > "$pidfile"
	echo "dev-chain: started pid $(cat "$pidfile"), log $log" >&2
	local waited=0 limit=$(((wait_blocks + 10) * block_ms / 1000 + 120)) best=""
	while :; do
		running || { tail -20 "$log" >&2; rm -f "$pidfile"; die "the node exited"; }
		best=$(best_block || true)
		if [ -n "$best" ] && [ "$best" -ge "$wait_blocks" ]; then break; fi
		[ "$waited" -ge "$limit" ] && die "no block $wait_blocks after $waited s; see $log"
		sleep 2
		waited=$((waited + 2))
	done
	echo "dev-chain: ready at ws://127.0.0.1:$port, block $best, ${block_ms} ms blocks"
}

stop() {
	if ! running; then
		rm -f "$pidfile"
		echo "dev-chain: not running"
		return
	fi
	local pid
	pid=$(cat "$pidfile")
	kill "$pid"
	for _ in $(seq 1 30); do kill -0 "$pid" 2> /dev/null || break; sleep 1; done
	kill -0 "$pid" 2> /dev/null && kill -9 "$pid"
	rm -f "$pidfile"
	echo "dev-chain: stopped pid $pid"
}

status() {
	if running; then
		echo "dev-chain: running (pid $(cat "$pidfile")) at ws://127.0.0.1:$port, best block $(best_block || echo unknown)"
	else
		echo "dev-chain: not running"
		return 1
	fi
}

case "${1:-}" in
	start) start ;;
	stop) stop ;;
	status) status ;;
	*) echo "usage: $0 start|stop|status" >&2; exit 2 ;;
esac
