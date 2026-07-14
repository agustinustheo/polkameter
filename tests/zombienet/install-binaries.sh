#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
#
# Download the small, pinned native toolchain needed by the Zombienet CLI
# smoke test. The full Polkadot node bundle is intentional: recent SDK nodes
# require the prepare and execute workers beside the main executable.
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TOOLS_DIR="${POLKAMETER_ZOMBIENET_TOOLS_DIR:-$ROOT/target/zombienet-tools/bin}"
POLKADOT_RELEASE="polkadot-stable2606"
ZOMBIENET_RELEASE="v0.4.14"

case "$(uname -s)-$(uname -m)" in
	Linux-x86_64)
		POLKADOT_SUFFIX=""
		ZOMBIENET_ASSET="zombie-cli-x86_64-unknown-linux-musl"
		ZOMBIENET_SHA256="0603d8d92afe20e7790e9c543eb9a10ab25eda46e8b9ae83704afb7743063bbd"
		POLKADOT_SHA256="43370cb48685fcbe7a97d73c1842f2c8a54c2aa5de156ea51ae769671d634cf6"
		PREPARE_SHA256="00a8a2a81a7ea5b536d3abf272e4cde3bfc93118a68e258424b8d7bb33fee756"
		EXECUTE_SHA256="efa276c71c952cbdd49cca22175cb218035698d3f54659289872252fc28f5c7d"
		;;
	Darwin-arm64)
		POLKADOT_SUFFIX="-aarch64-apple-darwin"
		ZOMBIENET_ASSET="zombie-cli-aarch64-apple-darwin"
		ZOMBIENET_SHA256="eff79014ae96090b0a2c539119f194bd78716da07613bb11e8480d6134e8fb2d"
		POLKADOT_SHA256="62df1f35b10b24951e6377c756503acebe4df39114e3fb2de0386c48f24c56c1"
		PREPARE_SHA256="a5c6f592d549a964bf82d9e00813dbb19812091caae4d5321bead3ce5a268adf"
		EXECUTE_SHA256="c573cb384b54724b29a4cf47fc8ffc352f0e6ae04c1321f9bc5290357969d2c0"
		;;
	*)
		echo "unsupported platform for the native Zombienet smoke test: $(uname -s)-$(uname -m)" >&2
		exit 1
		;;
esac

download() {
	local name="$1"
	local url="$2"
	local expected="$3"
	local destination="$TOOLS_DIR/$name"
	local actual
	if [ -x "$destination" ]; then
		actual="$(shasum -a 256 "$destination" | awk '{print $1}')"
		if [ "$actual" = "$expected" ]; then
			return
		fi
		rm -f "$destination"
	fi
	curl --fail --location --retry 3 --retry-delay 3 --output "$destination" "$url"
	actual="$(shasum -a 256 "$destination" | awk '{print $1}')"
	if [ "$actual" != "$expected" ]; then
		rm -f "$destination"
		echo "sha256 mismatch for $name: expected $expected, got $actual" >&2
		exit 1
	fi
	chmod +x "$destination"
}

mkdir -p "$TOOLS_DIR"
POLKADOT_BASE="https://github.com/paritytech/polkadot-sdk/releases/download/$POLKADOT_RELEASE"
ZOMBIENET_BASE="https://github.com/paritytech/zombienet-sdk/releases/download/$ZOMBIENET_RELEASE"
download polkadot "$POLKADOT_BASE/polkadot$POLKADOT_SUFFIX" "$POLKADOT_SHA256"
download polkadot-prepare-worker \
	"$POLKADOT_BASE/polkadot-prepare-worker$POLKADOT_SUFFIX" "$PREPARE_SHA256"
download polkadot-execute-worker \
	"$POLKADOT_BASE/polkadot-execute-worker$POLKADOT_SUFFIX" "$EXECUTE_SHA256"
download zombie-cli "$ZOMBIENET_BASE/$ZOMBIENET_ASSET" "$ZOMBIENET_SHA256"

printf '%s\n' "$TOOLS_DIR"
