#!/bin/sh
# Install a verified CLI release. Usage: sh install.sh [VERSION|latest]
set -eu

fail() { printf 'polkameter: %s\n' "$*" >&2; exit 1; }
[ "$#" -le 1 ] || fail 'usage: install.sh [VERSION|latest]'
version=${1:-${POLKAMETER_VERSION:-latest}}
base=${POLKAMETER_RELEASE_BASE_URL:-https://github.com/agustinustheo/polkameter/releases}
base=${base%/}
install_dir=${POLKAMETER_INSTALL_DIR:-"$HOME/.local/bin"}
command -v curl >/dev/null 2>&1 || fail 'curl is required'
case "$(uname -s)" in
    Linux) os=unknown-linux-gnu ;;
    Darwin) os=apple-darwin ;;
    *) fail 'supported systems: Linux and macOS; download the Windows zip from GitHub releases' ;;
esac
case "$(uname -m)" in
    x86_64|amd64) arch=x86_64 ;;
    aarch64|arm64) arch=aarch64 ;;
    *) fail 'supported architectures: x86_64 and aarch64' ;;
esac
if [ "$version" = latest ]; then
    url=$(curl -fsSLI -o /dev/null -w '%{url_effective}' "$base/latest")
    version=${url##*/}
fi
version=${version#v}
# Permit SemVer prerelease/build identifiers; reject paths and shell metacharacters.
case "$version" in
    ''|*[!0-9A-Za-z.+-]*) fail 'invalid version' ;;
esac
printf '%s\n' "$version" | LC_ALL=C grep -Eq '^[0-9]+\.[0-9]+\.[0-9]+([+-][0-9A-Za-z.+-]+)?$' || fail 'expected VERSION such as 0.2.0'
asset=polkameter-$version-$arch-$os.tar.gz
temporary=$(mktemp -d)
trap 'rm -rf "$temporary"' 0
trap 'exit 1' HUP INT TERM
curl -fsSL --retry 3 "$base/download/v$version/$asset" -o "$temporary/$asset"
curl -fsSL --retry 3 "$base/download/v$version/$asset.sha256" -o "$temporary/$asset.sha256"
read -r expected filename < "$temporary/$asset.sha256"
[ "$filename" = "$asset" ] || fail 'unexpected checksum filename'
case "$expected" in
    ''|*[!0-9a-fA-F]*) fail 'invalid SHA-256 checksum' ;;
esac
[ "${#expected}" -eq 64 ] || fail 'invalid SHA-256 checksum'
if command -v sha256sum >/dev/null 2>&1; then
    actual=$(sha256sum "$temporary/$asset")
elif command -v shasum >/dev/null 2>&1; then
    actual=$(shasum -a 256 "$temporary/$asset")
else
    fail 'sha256sum or shasum is required'
fi
actual=${actual%% *}
[ "$actual" = "$expected" ] || fail 'SHA-256 verification failed'
if [ "${POLKAMETER_VERIFY_ATTESTATION:-0}" = 1 ]; then
    command -v gh >/dev/null 2>&1 || fail 'gh is required to verify attestations'
    gh attestation verify "$temporary/$asset" --repo agustinustheo/polkameter
fi
# Extract only the executable; no archive path can choose the installation location.
tar -xzf "$temporary/$asset" -C "$temporary" polkameter
mkdir -p "$install_dir"
install -m 755 "$temporary/polkameter" "$install_dir/polkameter"
printf 'Installed PolkaMeter %s to %s/polkameter\n' "$version" "$install_dir"
printf 'Add %s to PATH to run polkameter.\n' "$install_dir"
