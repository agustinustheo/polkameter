# Install

Each method below installs the `polkameter` CLI on Linux (x86-64 or ARM64) or macOS (Apple silicon or Intel). On Windows, use WSL. The install script and the release archives also install the [substrate plugin](substrate-plugin.md), `polkameter-substrate-plugin`. `cargo binstall` and `cargo install` do not.

## Use the install script

You don't need Rust for this method.

```sh
curl -fsSL https://raw.githubusercontent.com/agustinustheo/polkameter/main/install.sh | sh
export PATH="$HOME/.local/bin:$PATH"
polkameter --version
```

The script needs `curl`, `tar`, and `sha256sum` or `shasum`. It checks the archive's SHA-256 checksum before it installs anything.

### Choose a version or directory

Pass a version, such as `0.3.0`, to install that release instead of the latest one. Set `POLKAMETER_INSTALL_DIR` to change the directory. The default is `~/.local/bin`.

```sh
curl -fsSL https://raw.githubusercontent.com/agustinustheo/polkameter/main/install.sh |
  POLKAMETER_INSTALL_DIR="$HOME/bin" sh -s -- <version>
```

If `gh` is signed in, the script also checks GitHub build provenance. Set `POLKAMETER_VERIFY_ATTESTATION=1` to require it.

## Use cargo binstall

With [cargo-binstall](https://github.com/cargo-bins/cargo-binstall), download the prebuilt CLI:

```sh
cargo binstall polkameter
```

Add `--version <version>` to select a release.

## Build from source

Use Rust 1.93 or newer:

```sh
cargo install --locked polkameter
```

This installs only the CLI, in `~/.cargo/bin`. A source build can take 10 minutes or more. Install the plugins your plan uses with [`polkameter plugin install`](cli.md#plugin).

## Download release archives

Download the archive for your operating system and processor from [GitHub releases](https://github.com/agustinustheo/polkameter/releases). Download the matching `.sha256` file too.

| System | Target | Format |
| --- | --- | --- |
| Linux x86-64 | `x86_64-unknown-linux-gnu` | `.tar.gz` |
| Linux ARM64 | `aarch64-unknown-linux-gnu` | `.tar.gz` |
| macOS Intel | `x86_64-apple-darwin` | `.tar.gz` |
| macOS Apple silicon | `aarch64-apple-darwin` | `.tar.gz` |

The archive is named `polkameter-<version>-<target>`, such as `polkameter-0.3.0-x86_64-unknown-linux-gnu.tar.gz`. It holds `polkameter` and `polkameter-substrate-plugin`. Linux builds need glibc 2.35 or newer, as in Ubuntu 22.04 and Debian 12. macOS builds need macOS 11 or newer.

Check the archive before you extract it:

- On Linux, run `sha256sum -c <archive-filename>.sha256`.
- On macOS, run `shasum -a 256 -c <archive-filename>.sha256`.
