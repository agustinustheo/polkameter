<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/agustinustheo/polkameter/main/docs/logo-dark.png">
    <img src="https://raw.githubusercontent.com/agustinustheo/polkameter/main/docs/logo-light.png" alt="PolkaMeter" width="380">
  </picture>
</p>

# PolkaMeter

<p align="center">
  <b>Load and stress tests for Polkadot SDK chains, with verdicts and reports for CI.</b>
</p>

<p align="center">
  <a href="https://github.com/agustinustheo/polkameter/actions/workflows/ci.yml?query=branch%3Amain"><img alt="Build" src="https://img.shields.io/github/actions/workflow/status/agustinustheo/polkameter/ci.yml?branch=main&label=build&logo=githubactions&logoColor=white"></a>
  <a href="https://github.com/agustinustheo/polkameter/releases/latest"><img alt="Release" src="https://img.shields.io/github/v/release/agustinustheo/polkameter?logo=github&color=E6007A"></a>
  <a href="https://crates.io/crates/polkameter"><img alt="crates.io: polkameter" src="https://img.shields.io/crates/v/polkameter?label=polkameter&logo=rust&color=E6007A"></a>
  <a href="https://crates.io/crates/polkameter-plugin-sdk"><img alt="crates.io: polkameter-plugin-sdk" src="https://img.shields.io/crates/v/polkameter-plugin-sdk?label=polkameter-plugin-sdk&logo=rust&color=E6007A"></a>
  <a href="https://docs.rs/polkameter-plugin-sdk"><img alt="docs.rs" src="https://img.shields.io/docsrs/polkameter-plugin-sdk?logo=docsdotrs"></a>
  <a href="https://github.com/agustinustheo/polkameter/blob/main/LICENSE"><img alt="License: Apache-2.0" src="https://img.shields.io/badge/license-Apache--2.0-blue"></a>
  <br>
  <img alt="Rust 1.93+" src="https://img.shields.io/badge/rust-1.93%2B-F74C00?logo=rust&logoColor=white">
  <img alt="Linux x86-64 | arm64" src="https://img.shields.io/badge/linux-x86--64%20%7C%20arm64-555?logo=linux&logoColor=white">
  <img alt="macOS arm64 | x86-64" src="https://img.shields.io/badge/macOS-arm64%20%7C%20x86--64-555?logo=apple&logoColor=white">
  <img alt="Windows x86-64" src="https://img.shields.io/badge/windows-x86--64-555">
  <br>
  <a href="https://github.com/agustinustheo/polkameter/stargazers"><img alt="Stars" src="https://img.shields.io/github/stars/agustinustheo/polkameter?style=flat&logo=github"></a>
  <a href="https://github.com/agustinustheo/polkameter/graphs/contributors"><img alt="Contributors" src="https://img.shields.io/github/contributors/agustinustheo/polkameter"></a>
  <a href="https://github.com/agustinustheo/polkameter/commits/main"><img alt="Last commit" src="https://img.shields.io/github/last-commit/agustinustheo/polkameter"></a>
</p>

---

PolkaMeter loads a Polkadot SDK chain with signed transactions at rising rates and tells you where it stops keeping up. You choose the transaction rates in an XML plan (`*.polkameter.xml`). A plugin prepares transactions for your runtime.

A finished load test gives a verdict: `PASS`, `WARN`, `FAIL` or `INCONCLUSIVE`. Every run also ends with an exit code. Both are made for CI.

## Highlights

- Built for CI. Every run has a documented exit code. The GitHub Action fails the job on `FAIL` or `INCONCLUSIVE`, uploads the run's reports and puts `summary.md` in the job summary.
- No plugin code for standard runtimes. The [substrate plugin](https://agustinustheodorus.com/polkameter/substrate-plugin.html#limits) signs remarks and transfers for compatible runtimes. `polkameter init --rpc <endpoint>` writes a starter plan.
- Finds the limit. It increases the rate in steps to find the first test limit the chain exceeds, then checks how fast the chain recovers.
- Transaction loss accounting. Each sent flood transaction gets one final status, such as finalized, refused or lost. PolkaMeter reports loss only when finalized-chain and ready-pool evidence is sufficient.
- Reports CI can read: `junit.xml` for test reports, `summary.md` and `summary.json`, and `samples.jtl` in the CSV format of JMeter, a popular load-testing tool. Secrets stay out of plans, and PolkaMeter [redacts](https://agustinustheodorus.com/polkameter/results.html#redaction) them from its reports and logs.

## Install

The install script downloads the CLI and the substrate plugin for Linux or macOS, checks the checksum and installs both in `~/.local/bin`:

```sh
curl -fsSL https://raw.githubusercontent.com/agustinustheo/polkameter/main/install.sh | sh
export PATH="$HOME/.local/bin:$PATH"
polkameter --version
```

[Install](https://agustinustheodorus.com/polkameter/install.html) also covers `cargo binstall`, the container and the release archives.

## Tour

Let's start a dev chain and load it. The dev chain script lives in the repository, so run the tour from a checkout. It needs Linux on x86-64 or macOS on Apple silicon:

```sh
git clone https://github.com/agustinustheo/polkameter && cd polkameter
cargo build --locked -p polkameter -p polkameter-substrate-plugin
export PATH="$PWD/target/debug:$PATH"
```

1. Start a dev chain. `scripts/dev-chain.sh` downloads a pinned node and runtime, checks them, and starts a chain with 6-second blocks where `//Alice` holds funds:

   ```console
   $ scripts/dev-chain.sh start
   ...
   dev-chain: ready at ws://127.0.0.1:9944, block 21, 6000 ms blocks
   ```

2. Install the plugin and map the seed. A plan names a credential profile. The plugin registry maps it to an environment variable on this machine:

   ```console
   $ polkameter plugin install "$(command -v polkameter-substrate-plugin)"
   Installed plugin substrate 0.3.0 (5 operations) from ~/polkameter/target/debug/polkameter-substrate-plugin
   $ polkameter plugin credential substrate-dev POLKAMETER_SUBSTRATE_SEED
   Credential profile substrate-dev reads POLKAMETER_SUBSTRATE_SEED.
   $ export POLKAMETER_SUBSTRATE_SEED='//Alice'
   ```

3. Write and check a plan. `init` checks the runtime's calls and measures the block time. `preflight` starts the plugin and contacts the chain without sending anything:

   ```console
   $ polkameter init --rpc ws://127.0.0.1:9944 --output plans/load.polkameter.xml
   Wrote plans/load.polkameter.xml for Custom (parachain-template-runtime 1, 6 s blocks). Next: polkameter validate plans/load.polkameter.xml
   $ polkameter validate plans/load.polkameter.xml
   Plan is structurally valid. Preflight checks the installed plugins.
   $ polkameter preflight plans/load.polkameter.xml
   Preflight passed.
   ```

4. Run it. The run writes its reports to a new directory under `runs/`:

   ```console
   $ polkameter run plans/load.polkameter.xml --output runs
   ...
   #### Result

   - **Verdict:** PASS
   - **Failure onset** (first failed load step): no measure violated
   - **Max sustained:** 50 tx/s target (step 4), 50 tx/s included
   ...
   - **Loss check:** 4500 sent: 4500 finalized (0 failed after inclusion), 0 refused, 0 still ready in the pool, 0 seen in a block but not verified final, **0 lost**, 0 unknown. ...
   ...
   Run completed. Artifacts: ~/polkameter/runs/run-1794021000609-68835-1
   ```

The generated plan ramps from 10 to 50 transactions per second, so a dev chain passes it. To find the limit, raise the rates. Stop the chain with `scripts/dev-chain.sh stop`.

[Stress-test a local dev chain](https://agustinustheodorus.com/polkameter/dev-chain.html) runs a plan that does find the limit. [Read the results](https://agustinustheodorus.com/polkameter/reading-results.html) explains each line of `summary.md`.

## In CI

The action installs the CLI, your plugins and credentials, runs the plan and uploads the run's reports. Save this as `.github/workflows/load-test.yml`:

```yaml
name: load-test
on: workflow_dispatch

jobs:
  load-test:
    runs-on: ubuntu-22.04
    steps:
      - uses: actions/checkout@v7
      - id: load
        uses: agustinustheo/polkameter@v0
        with:
          plan: <plan>
          plugins: plugins/<plugin-binary>
          fail-on: verdict
          credentials: <profile>=POLKAMETER_SETUP_SURI
        env:
          POLKAMETER_SETUP_SURI: ${{ secrets.SETUP_SURI }}
```

[Run in CI](https://agustinustheodorus.com/polkameter/ci.html) explains what fails the job and has jobs for GitLab CI and Jenkins.

## For AI agents

The repository is a Claude Code plugin marketplace. In a Claude Code session:

```text
/plugin marketplace add agustinustheo/polkameter
/plugin install polkameter@polkameter
```

Then ask for a load test of your chain. [Use PolkaMeter with an AI assistant](https://agustinustheodorus.com/polkameter/ai-assistants.html) covers other assistants and [`llms.txt`](https://agustinustheodorus.com/polkameter/llms.txt).

## Write your own plugin

If the substrate plugin doesn't fit your runtime, write a plugin. A plugin is a separate executable that exchanges one JSON message per line with PolkaMeter, so it lives in your repository. In Rust, it depends on [`polkameter-plugin-sdk`](https://docs.rs/polkameter-plugin-sdk). [Writing a plugin](https://agustinustheodorus.com/polkameter/writing-a-plugin.html) walks through a first one.

## Documentation

The book is at <https://agustinustheodorus.com/polkameter/>. Start with [Getting started](https://agustinustheodorus.com/polkameter/getting-started.html), which needs no chain, or [Troubleshooting](https://agustinustheodorus.com/polkameter/troubleshooting.html) if something broke.

To build and test PolkaMeter, see [`AGENTS.md`](https://github.com/agustinustheo/polkameter/blob/main/AGENTS.md#build-and-test). It also has the [repository layout](https://github.com/agustinustheo/polkameter/blob/main/AGENTS.md#layout). Release steps are in [`RELEASING.md`](https://github.com/agustinustheo/polkameter/blob/main/RELEASING.md), and changes in [`CHANGELOG.md`](https://github.com/agustinustheo/polkameter/blob/main/CHANGELOG.md).

## License

PolkaMeter is licensed under the [Apache License 2.0](https://github.com/agustinustheo/polkameter/blob/main/LICENSE).

<sub>Polkadot is a trademark of the Web3 Foundation. PolkaMeter is an independent project and is not endorsed by the Web3 Foundation.</sub>
