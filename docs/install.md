# Install

Install the currently published CLI with Rust 1.93 or newer:

<!-- TODO(v0.3.0): lead with the prebuilt installer once v0.3.0 assets are published. -->
```sh
cargo install --locked polkameter
polkameter --version
```

```text
polkameter 0.2.0
```

## Use the install script

> **Warning:** Prebuilt CLI archives and this script are available from v0.3.0. The latest release, v0.2.0, has no prebuilt CLI archive; use Cargo above until v0.3.0 is published.

Install the CLI binary without a Rust toolchain on Linux or macOS:

```sh
curl -fsSL https://raw.githubusercontent.com/agustinustheo/polkameter/main/install.sh | sh
export PATH="$HOME/.local/bin:$PATH"
polkameter --version
```

### Choose a version or directory

Pass a version to select a release instead of the latest stable release. Set `POLKAMETER_INSTALL_DIR` on the `sh` process to choose a directory:

```sh
curl -fsSL https://raw.githubusercontent.com/agustinustheo/polkameter/main/install.sh |
  POLKAMETER_INSTALL_DIR="$HOME/bin" sh -s -- <version>
```

Use a version such as `0.3.0` or `v0.3.0`. The script verifies the archive's SHA-256 checksum before installing `polkameter`; the default directory is `~/.local/bin`. It needs `curl`, `tar`, and either `sha256sum` or `shasum`.

When `gh` is available, the script automatically verifies GitHub build provenance and stops if verification fails. Install and authenticate the GitHub CLI before running the script to use this check. It requires the signer workflow `agustinustheo/polkameter/.github/workflows/release.yml` and the source ref `refs/tags/v<version>`, matching the GitHub Action installer.

A checksum detects a damaged download, but an attacker who replaces both files can replace the checksum too. Attestation verification additionally ties the archive to that repository's release workflow and version tag; it does not audit the code. Without `gh`, the script prints that only the checksum was verified. Set `POLKAMETER_VERIFY_ATTESTATION=1` to require `gh`, or `POLKAMETER_VERIFY_ATTESTATION=0` to opt out explicitly:

```sh
curl -fsSL https://raw.githubusercontent.com/agustinustheo/polkameter/main/install.sh |
  POLKAMETER_VERIFY_ATTESTATION=1 sh
```

## Use cargo binstall

> **Warning:** Binstall metadata and matching archives are available from v0.3.0. v0.2.0 has neither; use `cargo install --locked polkameter` until v0.3.0 is published.

If you have [cargo-binstall](https://github.com/cargo-bins/cargo-binstall), download the matching prebuilt binary:

```sh
cargo binstall polkameter
```

To select a version with these assets, add `--version <version>`. The metadata points to the same GitHub release archives as the install script.

## Use a container

> **Warning:** GHCR images are available from v0.3.0; the commands below require a published image.

Run the CLI from `ghcr.io/agustinustheo/polkameter:<version>`. Stable releases also update `:latest`, `:<major>` and `:<major>.<minor>`; for example, a `0.3.0` release publishes `:0.3.0`, `:0.3`, `:0` and `:latest`. Prereleases use their full version tag only.

The image supports `linux/amd64` and `linux/arm64`, includes CA certificates, and runs as user `10001:10001`. Mount your plan and a plugin built for **Linux and the container's architecture**; a macOS or Windows executable cannot run inside it. Build plugins in `rust:1.93.0-bookworm` or another environment compatible with the runtime’s glibc 2.36. Create a writable output directory before running from your project root:

```sh
mkdir -p polkameter-runs
docker run --rm --user "$(id -u):$(id -g)" \
  -e HOME=/tmp -e POLKAMETER_PLUGIN_REGISTRY=/tmp/plugins.json \
  -v "$PWD:/work" -w /work \
  --entrypoint /bin/sh ghcr.io/agustinustheo/polkameter:<version> -ec '
    polkameter plugin install /work/target/release/<plugin-binary>
    polkameter run /work/plans/transfers.polkameter.xml \
      --output /work/polkameter-runs --fail-on-verdict
  '
```

For a version check, run `docker run --rm ghcr.io/agustinustheo/polkameter:<version> --version`. See [Run PolkaMeter in CI](ci.md#gitlab-ci) for a GitLab `image:` example.

## Build from source

Use Rust 1.93 or newer to compile the CLI from a published crates.io version:

```sh
cargo install --locked polkameter
```

To build the current repository code instead:

```sh
cargo install --locked --git https://github.com/agustinustheo/polkameter polkameter
```

Cargo installs the executable in `~/.cargo/bin` by default. You still need to build and install the plugins required by your plan; see [Getting started](getting-started.md).

## Download release archives

> **Warning:** CLI archives are available from v0.3.0; v0.2.0 only has the older release assets.

Choose a target and verify the archive using the [release asset reference](release-assets.md).
