<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/agustinustheo/polkameter/main/assets/logo-dark.svg">
    <img src="https://raw.githubusercontent.com/agustinustheo/polkameter/main/assets/logo.svg" alt="PolkaMeter: a gauge whose needle points into the red zone" width="820">
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

PolkaMeter loads a Polkadot SDK chain with signed transactions at rising rates and tells you where it stops keeping up. It is a command-line tool, modeled on JMeter: an XML plan (`*.polkameter.xml`) lists the steps, and plugins, which run as separate processes, prepare the transactions for your runtime.

A completed load test reconciles its sent transactions against the finalized chain and reports `PASS`, `WARN`, `FAIL` or `INCONCLUSIVE`. Every run records an execution status and exit code. The verdict, the exit code and the files in the run directory are made for a CI job.

## Highlights

- **CI first.** A documented exit code for every run, and a verdict for completed load tests. By default, the GitHub Action fails the job on `FAIL` or `INCONCLUSIVE`, uploads the run directory, and puts `summary.md` in the job summary.
- **Standard Substrate runtimes.** The substrate plugin signs remarks and transfers for runtimes with the standard `System` and `Balances` pallets, sr25519-compatible accounts and signatures, and transaction extensions supported by subxt. `polkameter init --rpc <endpoint>` writes a starter plan.
- **Finds the breaking point.** Ramps the rate step by step and reports the first failed step, the highest rate the chain sustained, and how fast it recovered after the load stopped.
- **Zero-loss accounting.** Every sent transaction ends with exactly one status, such as finalized, refused or lost, so a transaction the chain silently dropped shows up as lost. A state check confirms a sample of them on chain.
- **Results CI understands.** `junit.xml` for test reports, `samples.jtl` in JMeter's CSV format, and `summary.md` and `summary.json` with the verdict and the numbers.
- **Credential redaction.** Plans name credential profiles rather than storing their values. PolkaMeter redacts credential values and supported endpoint secrets from its reports and logs. [Redaction](https://agustinustheodorus.com/polkameter/results.html#redaction) lists the exclusions, including transformed secrets, keys in URL paths and plugin-written files.
- **Agent-friendly.** `--format json` prints one JSON object per line for `validate`, `preflight`, `run` and `report`. A Claude Code plugin and an Agent Skill set up a load test for you.

## Install

<!-- v0.3.0: remove after release -->
> **Unreleased:** The published version is v0.2.0. The install script, `cargo binstall`, the container, the substrate plugin, `polkameter init`, `junit.xml` and the `@v0` action tag arrive with v0.3.0. Until then, build the CLI and the substrate plugin from `main`:
>
> ```sh
> cargo install --locked --git https://github.com/agustinustheo/polkameter --branch main polkameter polkameter-substrate-plugin
> ```

From v0.3.0, the install script downloads the CLI and the substrate plugin for Linux or macOS, verifies the checksum, and installs both in `~/.local/bin`:

```sh
curl -fsSL https://raw.githubusercontent.com/agustinustheo/polkameter/main/install.sh | sh
export PATH="$HOME/.local/bin:$PATH"
polkameter --version
```

Other ways to install it:

| Method | Command | Notes |
| --- | --- | --- |
| Cargo | `cargo install --locked polkameter` | Builds the CLI only, without the substrate plugin. Rust 1.93 or newer |
| cargo-binstall | `cargo binstall polkameter` | Downloads the release archive. From v0.3.0 |
| Container | `docker run --rm ghcr.io/agustinustheo/polkameter:<version> --version` | `linux/amd64` and `linux/arm64`. From v0.3.0 |
| Release archives | [Releases](https://github.com/agustinustheo/polkameter/releases) | Linux x86-64 and arm64, macOS arm64 and x86-64, Windows x86-64. From v0.3.0 |
| GitHub Action | `uses: agustinustheo/polkameter@v0` | See [In CI](#in-ci). `@v0` from v0.3.0 |

[Install](https://agustinustheodorus.com/polkameter/install.html) covers versions, build provenance checks and source builds.

## Tour

The dev-chain script lives in the repository, so the tour runs from the root of a checkout. It needs Linux on x86-64 or macOS on Apple silicon. Build the CLI and the plugin there, or skip the build if you installed both above:

```sh
git clone https://github.com/agustinustheo/polkameter && cd polkameter
cargo build --locked -p polkameter -p polkameter-substrate-plugin
export PATH="$PWD/target/debug:$PATH"
```

**1. Start a dev chain.** `scripts/dev-chain.sh` downloads a pinned `polkadot-omni-node` and the parachain template runtime, checks their SHA-256 sums, and starts a chain with 6-second blocks where `//Alice` holds funds:

```console
$ scripts/dev-chain.sh start
dev-chain: downloading https://github.com/paritytech/polkadot-sdk/releases/download/polkadot-stable2609-1/polkadot-omni-node-aarch64-apple-darwin
dev-chain: downloading https://github.com/paritytech/polkadot-sdk-parachain-template/releases/download/v0.0.5/parachain_template_runtime.compact.compressed.wasm
dev-chain: started pid 38403, log .polkameter/dev-chain/node.log
dev-chain: ready at ws://127.0.0.1:9944, block 21, 6000 ms blocks
```

**2. Install the plugin and map the seed.** A plan names a credential profile; the registry maps it to an environment variable on this machine:

```console
$ polkameter plugin install "$(command -v polkameter-substrate-plugin)"
Installed plugin substrate 0.3.0 (5 operations) from ~/polkameter/target/debug/polkameter-substrate-plugin
$ polkameter plugin credential substrate-dev POLKAMETER_SUBSTRATE_SEED
{
  "configured": true
}
$ export POLKAMETER_SUBSTRATE_SEED='//Alice'
```

**3. Write, validate and preflight a plan.** `init` checks the runtime's calls and measures the block time. `preflight` starts the plugin and contacts the chain without sending anything:

```console
$ polkameter init --rpc ws://127.0.0.1:9944 --output plans/load.polkameter.xml
Wrote plans/load.polkameter.xml for Custom (parachain-template-runtime 1, 6 s blocks). Next: polkameter validate plans/load.polkameter.xml
$ polkameter validate plans/load.polkameter.xml
Plan is structurally valid. Preflight checks the installed plugins.
$ polkameter preflight plans/load.polkameter.xml
Preflight passed.
```

**4. Run it.** The run writes `summary.md`, `summary.json`, `junit.xml`, `samples.jtl` and the raw records to a new run directory under `runs/`:

```console
$ polkameter run plans/load.polkameter.xml --output runs
Phase: preflight
Phase: setup
Phase: measurement
Phase: baseline
baseline: 3 probes landed, p50 6.0 s, max 6.0 s
Phase: load
step 0 prepared: target 10 tx/s, sent 10 tx/s, 5 blocks, 9.9 tx/s included, reply p95 0.0 s, 1 replies pending, backlog 2
...
step 4 prepared: target 50 tx/s, sent 50 tx/s, 5 blocks, 39.9 tx/s included, reply p95 0.0 s, 1 replies pending, backlog 303
stop: rate cap: all 5 steps ran
Phase: recovery
...
#### Result

- **Verdict:** PASS
- **Failure onset** (first failed load step): no measure violated
- **Max sustained:** 50 tx/s target (step 4), 50 tx/s included
- **Load stop:** rate cap: all 5 steps ran
- **Recovery:** back 1 s after the load stopped (3 probes in a row landed within 12.0 s). Backlog at stop 303 txs, drained after 7 s, at 16.8 tx/s.
- **Loss check:** 4500 sent: 4500 finalized (0 failed after inclusion), 0 refused, 0 still ready in the pool, 0 seen in a block but not verified final, **0 lost**, 0 unknown. ...
...
Phase: teardown
Run completed. Artifacts: ~/polkameter/runs/run-1794021000609-68835-1
```

The generated plan ramps from 10 to 50 transactions per second, so a dev chain passes it. To find the limit, raise the rates. The remarks example, [`examples/substrate-remarks.polkameter.xml`](https://github.com/agustinustheo/polkameter/blob/main/examples/substrate-remarks.polkameter.xml), ramps to 600 transactions per second:

```console
$ polkameter run examples/substrate-remarks.polkameter.xml --output target/runs --fail-on-verdict
...
- **Verdict:** FAIL
- **Failure onset** (first failed load step): step 5 (600 tx/s): latency, p95 send -> best block 12.9 s
- **Max sustained:** 500 tx/s target (step 4), 500 tx/s included
- **Load stop:** rate cap: all 6 steps ran
- **Recovery:** back 1 s after the load stopped (3 probes in a row landed within 12.0 s). Backlog at stop 5712 txs, drained after 8 s, at 317.3 tx/s.
- **Loss check:** 63002 sent: 63002 finalized (0 failed after inclusion), 0 refused, 0 still ready in the pool, 0 seen in a block but not verified final, **0 lost**, 0 unknown. ...
...
| step | target tx/s | duration s | transactions sent | sent/s | included/s | included % | p50 s | p95 s (limit 10.0) | ... | max ours/block | max ref time % | max proof % |
...
| 4 | 500 | 30.0 | 15001 | 500 | 500 | 100.0 | 4.1 | 6.8 | ... | 3011 | 97.9 | 6.8 |
| 5 | 600 | 30.0 | 18001 | 600 | 600 | 100.0 | 6.2 | 12.9 | ... | 3075 | 100.0 | 6.9 |
...
PolkaMeter: verdict FAIL: step 5 broke its latency measure (p95 send -> best block 12.9 s); exit 1 because of --fail-on-verdict (the checks are in summary.md)
```

The chain held 500 transactions per second. At 600, blocks were full (100% ref time) and the p95 time to a block rose past the 10-second limit. All 63,002 transactions were finalized and none were lost. Stop the chain with `scripts/dev-chain.sh stop`.

[Stress-test a local dev chain](https://agustinustheodorus.com/polkameter/dev-chain.html) walks through this run, and [Read the results](https://agustinustheodorus.com/polkameter/reading-results.html) explains each line of `summary.md`.

## In CI

The composite action installs the CLI, installs your plugins and credentials, runs the plan and uploads the run directory. It supports Linux and macOS runners. Save this as `.github/workflows/load-test.yml`:

<!-- v0.3.0: remove after release (change @v0.2.0 to @v0) -->
```yaml
name: load-test
on: workflow_dispatch

jobs:
  load-test:
    runs-on: ubuntu-22.04
    steps:
      - uses: actions/checkout@v7
      - id: load
        uses: agustinustheo/polkameter@v0.2.0
        with:
          plan: <plan>
          plugins: plugins/<plugin-binary>
          fail-on: verdict
          credentials: <profile>=POLKAMETER_SETUP_SURI
        env:
          POLKAMETER_SETUP_SURI: ${{ secrets.SETUP_SURI }}
```

<!-- v0.3.0: remove after release -->
`@v0.2.0` builds the CLI from source. From v0.3.0, `@v0` downloads a verified release CLI and the default `fail-on` also fails the job on `INCONCLUSIVE`.

<!-- v0.3.0: remove after release -->
> **Warning:** The table below describes the unreleased CLI built from `main`. The `@v0.2.0` action above returns `1` when a run fails before setup and does not support `--fail-on-inconclusive`.

The action preserves the exit code of `polkameter run`:

| Exit code | Meaning |
| --- | --- |
| `0` | The run completed |
| `1` | The run failed after setup started, or the verdict was `FAIL` (with `--fail-on-verdict`) or `INCONCLUSIVE` (with `--fail-on-inconclusive`) |
| `2` | The plan or the arguments are invalid |
| `3` | A check before setup failed, so nothing was sent to the chain: a missing plugin or credential, or a failed preflight |
| `130` / `143` | Stopped by SIGINT / SIGTERM, for example when the job was cancelled |

[Run PolkaMeter in CI](https://agustinustheodorus.com/polkameter/ci.html) lists every input and output, GitHub test reports from `junit.xml`, and jobs for GitLab CI and other CI systems. [Stress-test a local dev chain](https://agustinustheodorus.com/polkameter/dev-chain.html#run-it-in-github-actions) has a complete job that starts a dev chain in the runner.

## For AI agents

The repository is a Claude Code plugin marketplace. In a Claude Code session:

```text
/plugin marketplace add agustinustheo/polkameter
/plugin install polkameter@polkameter
```

Then ask for a load test, for example: *"Stress-test the chain my node serves at ws://127.0.0.1:9944 with PolkaMeter, and add a CI job that runs a short check on every pull request."*

- **Without the marketplace:** copy [`skills/polkameter`](https://github.com/agustinustheo/polkameter/tree/main/skills/polkameter) into `.claude/skills/` in your project, or into another assistant's skills directory.
- **Other assistants:** point them at [`llms.txt`](https://agustinustheodorus.com/polkameter/llms.txt), an index of the documentation.
- **Working on PolkaMeter itself:** [`AGENTS.md`](https://github.com/agustinustheo/polkameter/blob/main/AGENTS.md) has the build and test commands and the conventions.

[Use PolkaMeter with an AI assistant](https://agustinustheodorus.com/polkameter/ai-assistants.html) has the details.

## Write your own plugin

If the substrate plugin does not cover your runtime, write a plugin. A plugin is an executable that speaks a JSON-line protocol on standard input and output, so it lives in your repository and installs without rebuilding PolkaMeter. In Rust, it depends on one crate:

```toml
[dependencies]
polkameter-plugin-sdk = { version = "0.3", features = ["chain"] }
```

<!-- v0.3.0: remove after release -->
Until v0.3.0 is on crates.io, use a Git dependency, as [the plugin SDK crate](https://agustinustheodorus.com/polkameter/plugins.html#the-plugin-sdk-crate) shows.

The default features give the protocol. `chain` adds RPC reads and transaction signing for any runtime, and `observers` adds run recording and checks. [Writing a plugin](https://agustinustheodorus.com/polkameter/writing-a-plugin.html) walks through a first plugin, and the API is on [docs.rs](https://docs.rs/polkameter-plugin-sdk).

## Documentation

The book is at <https://agustinustheodorus.com/polkameter/>.

- [Getting started](https://agustinustheodorus.com/polkameter/getting-started.html): install the CLI and run the example plan, no chain needed
- [Stress-test a local dev chain](https://agustinustheodorus.com/polkameter/dev-chain.html): the tour above, in full
- [Run a load test](https://agustinustheodorus.com/polkameter/load-test.html): point a plan at your chain
- [Read the results](https://agustinustheodorus.com/polkameter/reading-results.html): verdict, max sustained rate, loss check, recovery
- [Run PolkaMeter in CI](https://agustinustheodorus.com/polkameter/ci.html): the GitHub Action, GitLab and others
- [Troubleshooting](https://agustinustheodorus.com/polkameter/troubleshooting.html): errors as the CLI prints them, with fixes
- Reference: [Plan](https://agustinustheodorus.com/polkameter/plans.html), [CLI](https://agustinustheodorus.com/polkameter/cli.html), [Run directory](https://agustinustheodorus.com/polkameter/results.html), [Plugins and SDK](https://agustinustheodorus.com/polkameter/plugins.html), [The substrate plugin](https://agustinustheodorus.com/polkameter/substrate-plugin.html)
- [How a run works](https://agustinustheodorus.com/polkameter/how-it-works.html): phases, loss accounting and the crates

## Repository layout

| Path | What |
| --- | --- |
| [`crates/cli`](https://github.com/agustinustheo/polkameter/tree/main/crates/cli) | The `polkameter` CLI: plan engine, load runner, `init`, remote agent |
| [`crates/plugin-sdk`](https://github.com/agustinustheo/polkameter/tree/main/crates/plugin-sdk) | `polkameter-plugin-sdk`, the crate plugins build on |
| [`plugins/substrate`](https://github.com/agustinustheo/polkameter/tree/main/plugins/substrate) | The substrate plugin, `polkameter-substrate-plugin` |
| [`plugins/example`](https://github.com/agustinustheo/polkameter/tree/main/plugins/example) | An example plugin that needs no chain |
| [`schemas`](https://github.com/agustinustheo/polkameter/tree/main/schemas) | The plan XSD, `polkameter-plan.xsd` |
| [`examples`](https://github.com/agustinustheo/polkameter/tree/main/examples) | Example plans, including the substrate remarks and transfers plans |
| [`skills/polkameter`](https://github.com/agustinustheo/polkameter/tree/main/skills/polkameter) | The Agent Skill, with plan and CI templates |
| [`scripts`](https://github.com/agustinustheo/polkameter/tree/main/scripts) | `dev-chain.sh`, release, packaging and action helpers, `make_logo.py` |
| [`docs`](https://github.com/agustinustheo/polkameter/tree/main/docs) | The mdBook documentation |
| [`action.yml`](https://github.com/agustinustheo/polkameter/blob/main/action.yml) | The GitHub Action |
| [`install.sh`](https://github.com/agustinustheo/polkameter/blob/main/install.sh) | The install script |
| [`Dockerfile`](https://github.com/agustinustheo/polkameter/blob/main/Dockerfile) | The container image |

To build and test, see [`AGENTS.md`](https://github.com/agustinustheo/polkameter/blob/main/AGENTS.md#build-and-test). Release steps are in [`RELEASING.md`](https://github.com/agustinustheo/polkameter/blob/main/RELEASING.md), and changes in [`CHANGELOG.md`](https://github.com/agustinustheo/polkameter/blob/main/CHANGELOG.md).

## License

PolkaMeter is licensed under the [Apache License 2.0](https://github.com/agustinustheo/polkameter/blob/main/LICENSE).

<sub>Polkadot is a trademark of the Web3 Foundation. PolkaMeter is an independent project and is not endorsed by the Web3 Foundation.</sub>
