# Install

Install the CLI binary without a Rust toolchain on Linux or macOS:

```sh
curl -fsSL https://raw.githubusercontent.com/agustinustheo/polkameter/main/install.sh | sh
export PATH="$HOME/.local/bin:$PATH"
polkameter --version
```

These installation recipes use assets produced by the release workflow. They become available after a release containing this distribution setup; older releases may only contain the combined desktop bundles.

## Choose a version or directory

Pass a version to select a release instead of the latest stable release. Set `POLKAMETER_INSTALL_DIR` on the `sh` process to choose a directory:

```sh
curl -fsSL https://raw.githubusercontent.com/agustinustheo/polkameter/main/install.sh |
  POLKAMETER_INSTALL_DIR="$HOME/bin" sh -s -- <version>
```

Use a version such as `0.2.1` or `v0.2.1`. The script verifies the archive's SHA-256 checksum before installing `polkameter`; the default directory is `~/.local/bin`. It needs `curl`, `tar`, and either `sha256sum` or `shasum`.

To also verify GitHub's build provenance, install and authenticate the GitHub CLI (`gh`), then set `POLKAMETER_VERIFY_ATTESTATION=1` on the `sh` process. If verification fails, installation stops:

```sh
curl -fsSL https://raw.githubusercontent.com/agustinustheo/polkameter/main/install.sh |
  POLKAMETER_VERIFY_ATTESTATION=1 sh
```

## Use cargo binstall

If you have [cargo-binstall](https://github.com/cargo-bins/cargo-binstall), download the matching prebuilt binary:

```sh
cargo binstall polkameter
```

Binstall metadata is included in CLI crate versions published with this distribution setup. To select one, add `--version <version>`. The metadata points to the same GitHub release archives as the install script.

## Use a container

Run the CLI from `ghcr.io/agustinustheo/polkameter:<version>`. Stable releases also update `:latest`, `:<major>` and `:<major>.<minor>`; for example, a `0.2.1` release publishes `:0.2.1`, `:0.2`, `:0` and `:latest`. Prereleases use their full version tag only.

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
cargo +1.93.0 install --locked polkameter
```

To build the current repository code instead:

```sh
cargo +1.93.0 install --locked --git https://github.com/agustinustheo/polkameter polkameter
```

Cargo installs the executable in `~/.cargo/bin` by default. You still need to build and install the plugins required by your plan; see [Getting started](getting-started.md).

## Download release archives

Select your target from [GitHub releases](https://github.com/agustinustheo/polkameter/releases), then download the archive and its `.sha256` file. Linux binaries require glibc 2.35 or newer, as provided by Ubuntu 22.04 and Debian 12; Alpine's musl libc needs the container instead. macOS binaries target macOS 11 or newer.

| System | Target triple | Format |
| --- | --- | --- |
| Linux x86-64 | `x86_64-unknown-linux-gnu` | `.tar.gz` |
| Linux ARM64 | `aarch64-unknown-linux-gnu` | `.tar.gz` |
| macOS Intel | `x86_64-apple-darwin` | `.tar.gz` |
| macOS Apple Silicon | `aarch64-apple-darwin` | `.tar.gz` |
| Windows x86-64 | `x86_64-pc-windows-msvc` | `.zip` |

The asset naming contract for downloaders, including the GitHub Action, is:

- Release tag: `v<version>`, where `<version>` has no leading `v`.
- Unix archive: `polkameter-<version>-<target>.tar.gz`.
- Windows archive: `polkameter-<version>-x86_64-pc-windows-msvc.zip`.
- Checksum: the complete archive filename followed by `.sha256`, containing `<sha256>  <archive-filename>` and a newline.
- Archive root: only `polkameter` (`polkameter.exe` on Windows), `LICENSE` and `README.md`.
- Download URL: `https://github.com/agustinustheo/polkameter/releases/download/v<version>/<archive-filename>`.

On Linux, verify the checksum with `sha256sum -c <archive-filename>.sha256`; on macOS use `shasum -a 256 -c <archive-filename>.sha256`. On Windows use PowerShell's `Get-FileHash <archive-filename> -Algorithm SHA256` and compare it with the checksum file before extracting. The archives have GitHub build provenance attestations; `gh attestation verify <archive-filename> --repo agustinustheo/polkameter` verifies one.

Desktop assets use the separate name `polkameter-desktop-<version>-<platform>.tar.gz`. Download the CLI archives above for command-line use.
