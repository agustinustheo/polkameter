# Install

PolkaMeter runs on Linux (x86-64 or ARM64) and macOS (Apple silicon or Intel). On Windows, use WSL.

A load test on a standard runtime needs the CLI, `polkameter`, and the [substrate plugin](substrate-plugin.md), `polkameter-substrate-plugin`. The install script and the release archives install both. Cargo needs a second command for the plugin. Whichever you use, finish with [Register the plugin](#register-the-plugin).

## Use the install script

You don't need Rust for this method.

```sh
curl -fsSL https://raw.githubusercontent.com/agustinustheo/polkameter/main/install.sh | sh
export PATH="$HOME/.local/bin:$PATH"
polkameter --version
```

The script needs `curl`, `tar`, and `sha256sum` or `shasum`. It checks the archive's SHA-256 checksum before it installs anything. If `gh` is signed in, it also checks the build attestation, GitHub's proof that the release workflow built the archive. Set `POLKAMETER_VERIFY_ATTESTATION=1` to require that check.

To install a given release, pass its version. Set `POLKAMETER_INSTALL_DIR` to change the directory from `~/.local/bin`:

```sh
curl -fsSL https://raw.githubusercontent.com/agustinustheo/polkameter/main/install.sh |
  POLKAMETER_INSTALL_DIR="$HOME/bin" sh -s -- 0.4.3
```

## Use cargo

[cargo-binstall](https://github.com/cargo-bins/cargo-binstall) downloads the prebuilt CLI. `cargo install` builds it from source with Rust 1.93 or newer, which takes 10 minutes or more. The substrate plugin isn't on crates.io, so cargo builds it from the release tag. Both go to `~/.cargo/bin`:

```sh
cargo binstall polkameter --version 0.4.3    # or: cargo install --locked polkameter --version 0.4.3
cargo install --locked --git https://github.com/agustinustheo/polkameter --tag v0.4.3 polkameter-substrate-plugin
```

## Download release archives

Download `polkameter-<version>-<target>.tar.gz` and its `.sha256` file from [GitHub releases](https://github.com/agustinustheo/polkameter/releases). The targets are `x86_64-unknown-linux-gnu`, `aarch64-unknown-linux-gnu`, `x86_64-apple-darwin` (Intel) and `aarch64-apple-darwin` (Apple silicon). Linux builds need glibc 2.35 or newer, as in Ubuntu 22.04 and Debian 12. macOS builds need macOS 11 or newer.

Check the archive, then extract both executables:

```sh
sha256sum -c polkameter-0.4.3-<target>.tar.gz.sha256    # on macOS: shasum -a 256 -c
mkdir -p ~/.local/bin
tar -xzf polkameter-0.4.3-<target>.tar.gz -C ~/.local/bin polkameter polkameter-substrate-plugin
export PATH="$HOME/.local/bin:$PATH"
```

## Register the plugin

PolkaMeter runs only registered plugins. Register the substrate plugin now, and again after every upgrade or rebuild:

```sh
polkameter plugin install "$(command -v polkameter-substrate-plugin)"
```

[`plugin install`](cli.md#plugin) explains the registry.

## Versions

Use one release everywhere. The plan's `<plugin id="substrate" version="…">` must equal the installed plugin's version. That's also the CLI's version, which `polkameter --version` prints. Fetch `dev-chain.sh` and the example plans from that release's tag, such as `v0.4.3`. In CI, set the action's `version` input to it too.
