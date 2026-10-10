<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/agustinustheo/polkameter/main/docs/logo-dark.png">
    <img src="https://raw.githubusercontent.com/agustinustheo/polkameter/main/docs/logo-light.png" alt="Polkameter" width="380">
  </picture>
</p>

<p align="center">
  <a href="https://github.com/agustinustheo/polkameter/actions/workflows/ci.yml?query=branch%3Amain"><img src="https://github.com/agustinustheo/polkameter/actions/workflows/ci.yml/badge.svg?branch=main" alt="CI"></a>
  <a href="LICENSE"><img src="https://img.shields.io/github/license/agustinustheo/polkameter?style=flat-square" alt="Apache-2.0 license"></a>
  <a href="https://github.com/agustinustheo/polkameter/graphs/contributors"><img src="https://img.shields.io/github/contributors/agustinustheo/polkameter?style=flat-square" alt="Contributors"></a>
  <a href="https://github.com/agustinustheo/polkameter/stargazers"><img src="https://img.shields.io/github/stars/agustinustheo/polkameter?style=flat-square" alt="Stars"></a>
</p>

Polkameter is a load-testing tool for Polkadot SDK chains, modeled on JMeter: an XML plan composes steps, and installed plugins supply the runtime-specific operations. Polkameter submits the transactions a plugin prepares at a controlled rate, measures a baseline and recovery, reconciles every transaction against the finalized chain and the ready pool, scrapes the nodes, observes the relay for a parachain, and writes JMeter-compatible results.

It runs both ways: a headless `polkameter` CLI for CI and remote load machines, and a Tauri desktop app that edits and runs the same plans. Both drive the same Rust engine.

## Install

The command line tool is on crates.io (Rust 1.93 or newer), and it has no desktop dependencies:

```sh
cargo install polkameter
```

The desktop app is not on crates.io. Download it from [GitHub Releases](https://github.com/agustinustheo/polkameter/releases), with an AppImage or deb for Linux, a dmg for macOS and an installer for Windows.

To build from source, build the CLI and, for the desktop app, the frontend first:

```sh
cargo build --release -p polkameter --bin polkameter   # the CLI
corepack pnpm install && corepack pnpm build           # the frontend, only for the desktop app
corepack pnpm tauri dev                                # the desktop app, in development
```

## Command line

```sh
# Install a plugin; its manifest is read and its executable checksum pinned.
polkameter plugin install path/to/plugin

# Map a plan's credential profile and topology alias to this host.
polkameter plugin credential previewnet-sudo POLKAMETER_SETUP_SURI
polkameter plugin topology previewnet path/to/zombie.json

# Validate offline, preflight against the chain, run, and regenerate a report.
polkameter validate plan.polkameter.xml
polkameter preflight plan.polkameter.xml
polkameter run plan.polkameter.xml --output runs
polkameter report runs/run-<id>

# Run on a remote agent that has its own plugins and credentials.
polkameter agent serve --bind 127.0.0.1:9901 --output-root /data/runs
polkameter run plan.polkameter.xml --remote http://127.0.0.1:9901 --remote-token-env AGENT_TOKEN
```

`--format json` prints machine-readable events. A run exits nonzero on a tool or setup failure, a failed assertion, or, in smoke mode, a monitor problem or a required check without a result.

## Plans, plugins and results

[The plugin guide](https://github.com/agustinustheo/polkameter/blob/main/docs/plugins.md) describes the plan format, the execution order, measured load, monitors, plugin observers and checks, the plugin protocol and the result files. [The schema](https://github.com/agustinustheo/polkameter/blob/main/schemas/polkameter-plan.xsd) defines the XML and [the example](https://github.com/agustinustheo/polkameter/blob/main/examples/plugin-workflow.polkameter.xml) runs without a chain. `scripts/local-fork-run.sh` runs a plan on a fresh local Zombienet network.

Each run directory holds `samples.jtl` (JMeter CSV), `events.jsonl`, `execution.json`, plots and, for a measured load, `transactions.jsonl`, raw node scrapes, `summary.json` and `summary.md`.

## Checks

```sh
corepack pnpm test
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build -p polkameter --bin polkameter
cargo publish --workspace --exclude polkameter-desktop --exclude polkameter-example-plugin --dry-run
cargo build -p polkameter-example-plugin
tests/cli-smoke.sh
```
