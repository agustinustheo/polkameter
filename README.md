<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/agustinustheo/polkameter/main/docs/logo-dark.png">
    <img src="https://raw.githubusercontent.com/agustinustheo/polkameter/main/docs/logo-light.png" alt="Polkameter" width="380">
  </picture>
</p>

<p align="center">
  <a href="https://github.com/agustinustheo/polkameter/actions/workflows/ci.yml?query=branch%3Amain"><img src="https://github.com/agustinustheo/polkameter/actions/workflows/ci.yml/badge.svg?branch=main" alt="CI"></a>
  <a href="https://crates.io/crates/polkameter"><img src="https://img.shields.io/crates/v/polkameter?style=flat-square" alt="crates.io"></a>
  <a href="LICENSE"><img src="https://img.shields.io/github/license/agustinustheo/polkameter?style=flat-square" alt="Apache-2.0 license"></a>
  <a href="https://github.com/agustinustheo/polkameter/graphs/contributors"><img src="https://img.shields.io/github/contributors/agustinustheo/polkameter?style=flat-square" alt="Contributors"></a>
  <a href="https://github.com/agustinustheo/polkameter/stargazers"><img src="https://img.shields.io/github/stars/agustinustheo/polkameter?style=flat-square" alt="Stars"></a>
</p>

PolkaMeter is a load-testing tool for Polkadot SDK chains, modeled on JMeter. An XML plan lists the steps, and installed plugins supply the chain-specific operations. PolkaMeter submits the transactions a plugin prepares at a controlled rate, measures the chain before and after the load, checks every transaction against the finalized chain, and writes a verdict with JMeter-compatible results.

## Quick start

Install the published CLI with Rust 1.93 or newer:

<!-- TODO(v0.3.0): lead with the prebuilt installer after the first CLI-archive release is published. -->
```sh
cargo install --locked polkameter
polkameter --version
```

```text
polkameter 0.2.0
```

[Install](docs/install.md) covers version selection and source installation. Prebuilt CLI archives, `install.sh`, binstall metadata and containers are available **from v0.3.0**; v0.2.0 has no prebuilt CLI archives.

From a checkout of this repository, build the example plugin and install it. The example needs no chain:

```sh
cargo build -p polkameter-example-plugin
export POLKAMETER_PLUGIN_REGISTRY="$PWD/target/plugins.json"
polkameter plugin install target/debug/polkameter-example-plugin
```

Then validate, inspect and run the example plan:

```sh
polkameter validate examples/plugin-workflow.polkameter.xml
polkameter plugin inspect examples/plugin-workflow.polkameter.xml
polkameter run examples/plugin-workflow.polkameter.xml --output target/runs
```

The run writes a run directory under `target/runs`.

## GitHub Action

Run a plan in GitHub Actions without installing anything first. The action supports Linux and macOS runners. It runs the plan, uploads the run directory and ends the job with the CLI's exit code. From v0.3.0, it downloads a verified release CLI when available and otherwise builds from source.

```yaml
- uses: actions/checkout@v7
- uses: agustinustheo/polkameter@v0.2.0   # source build; @v0 is available from v0.3.0
  with:
    plan: plans/transfers.polkameter.xml
    credentials: previewnet-sudo=POLKAMETER_SETUP_SURI
  env:
    POLKAMETER_SETUP_SURI: ${{ secrets.SETUP_SURI }}
```

After a successful stable release, `v0` follows the latest `0.x` release commit. Pin a full commit SHA for an immutable action version. [Run PolkaMeter in CI](https://agustinustheodorus.com/polkameter/ci.html) lists every input and output, including container-based GitLab jobs.

## Documentation

The documentation site has the getting-started tutorial, the plan, CLI and results references, and the plugin guide: <https://agustinustheodorus.com/polkameter/>

Plugins are written in Rust against one crate, [`polkameter-plugin-sdk`](https://crates.io/crates/polkameter-plugin-sdk). Its `chain` and `observers` features add chain access and run recording; see [the plugin SDK crate](https://agustinustheodorus.com/polkameter/plugins.html#the-plugin-sdk-crate).

## Development

Run these from a checkout, with Rust 1.93 or newer, before you open a pull request. They are the core of CI; [`ci.yml`](.github/workflows/ci.yml) also runs the frontend tests, the plan, distribution and action tests, the dependency audit, the crates.io dry run and the desktop and container builds.

```sh
cargo fmt --all -- --check
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy -p polkameter-plugin-sdk --no-default-features --features chain --all-targets -- -D warnings
cargo build -p polkameter --bin polkameter
cargo build -p polkameter-example-plugin
tests/cli-smoke.sh
```

The workspace build turns on every feature of `polkameter-plugin-sdk`, so the second `clippy` line checks the SDK with one feature set alone. CI runs it with no features, `chain` and `observers`.

`tests/cli-smoke.sh` uses the two binaries built above. It runs the example plan locally and through a loopback remote agent.
