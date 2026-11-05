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
fetch() {
    # Keep HTTP failures separate from transport errors, so missing old assets
    # and GitHub's rate limit do not end in a bare curl error under set -e.
    if ! status=$(curl -sSL --retry 3 -w '%{http_code}' "$1" -o "$2"); then
        fail "could not download $1; check your connection and retry"
    fi
    case "$status" in
        200) ;;
        404)
            if [ "$3" = archive ]; then
                fail "v$version has no prebuilt CLI archive; install with cargo install --locked polkameter, or pick a newer version"
            fi
            fail "GitHub returned 404 for $1; check the release version and asset availability"
            ;;
        403|429) fail "GitHub returned HTTP $status (rate limit or access denied); retry later or install with cargo install --locked polkameter" ;;
        *) fail "GitHub returned HTTP $status for $1" ;;
    esac
}
if [ "$version" = latest ]; then
    if ! response=$(curl -sSLI -o /dev/null -w '%{http_code} %{url_effective}' "$base/latest"); then
        fail 'could not resolve the latest release; check your connection and retry'
    fi
    status=${response%% *}
    url=${response#* }
    case "$status" in
        200) ;;
        404) fail 'GitHub returned 404: no latest stable release was found; select a version or install with cargo install --locked polkameter' ;;
        403|429) fail "GitHub returned HTTP $status (rate limit or access denied); retry later or select a version" ;;
        *) fail "GitHub returned HTTP $status while resolving the latest release" ;;
    esac
    version=${url##*/}
    case "$version" in
        *-*) fail 'latest resolved to a prerelease; select that version explicitly' ;;
    esac
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
fetch "$base/download/v$version/$asset" "$temporary/$asset" archive
fetch "$base/download/v$version/$asset.sha256" "$temporary/$asset.sha256" checksum
read -r expected filename < "$temporary/$asset.sha256" || fail 'invalid checksum file (expected one line ending in a newline)'
[ "$filename" = "$asset" ] || fail 'invalid checksum file: unexpected filename'
case "$expected" in
    ''|*[!0-9a-fA-F]*) fail 'invalid checksum file: invalid SHA-256 checksum' ;;
esac
[ "${#expected}" -eq 64 ] || fail 'invalid checksum file: invalid SHA-256 checksum'
if command -v sha256sum >/dev/null 2>&1; then
    actual=$(sha256sum "$temporary/$asset")
elif command -v shasum >/dev/null 2>&1; then
    actual=$(shasum -a 256 "$temporary/$asset")
else
    fail 'sha256sum or shasum is required'
fi
actual=${actual%% *}
[ "$actual" = "$expected" ] || fail 'SHA-256 verification failed'
verify=${POLKAMETER_VERIFY_ATTESTATION:-auto}
case "$verify" in
    0) printf 'polkameter: attestation verification disabled; only the checksum was verified.\n' >&2 ;;
    auto|1)
        if command -v gh >/dev/null 2>&1; then
            # gh cannot query attestations anonymously, even for a public repository.
            # It exits 4 when it has no github.com credentials (gh help exit-codes).
            status=0
            gh attestation verify "$temporary/$asset" --repo agustinustheo/polkameter --hostname github.com \
                --signer-workflow agustinustheo/polkameter/.github/workflows/release.yml \
                --source-ref "refs/tags/v$version" || status=$?
            case "$status:$verify" in
                0:*) ;;
                4:1) fail 'gh must be authenticated to github.com to verify attestations; run gh auth login or set GH_TOKEN' ;;
                4:auto) printf 'polkameter: gh is not authenticated to github.com; only the checksum was verified. Run gh auth login or set GH_TOKEN to verify the build attestation.\n' >&2 ;;
                *) fail 'attestation verification failed; see the release recovery and trust documentation' ;;
            esac
        elif [ "$verify" = 1 ]; then
            fail 'gh is required to verify attestations'
        else
            printf 'polkameter: gh is unavailable; only the checksum was verified.\n' >&2
        fi
        ;;
    *) fail 'POLKAMETER_VERIFY_ATTESTATION must be auto, 1, or 0' ;;
esac
# Extract only the executable; no archive path can choose the installation location.
tar -xzf "$temporary/$asset" -C "$temporary" polkameter
mkdir -p "$install_dir"
install -m 755 "$temporary/polkameter" "$install_dir/polkameter"
printf 'Installed PolkaMeter %s to %s/polkameter\n' "$version" "$install_dir"
printf 'Add %s to PATH to run polkameter.\n' "$install_dir"
