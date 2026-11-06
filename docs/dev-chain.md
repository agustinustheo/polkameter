# Stress-test a local dev chain

You will start a Polkadot SDK dev chain with one script, load it with the [substrate plugin](substrate-plugin.md), and find the rate at which it stops keeping up. The same steps run in CI.

The script runs `polkadot-omni-node` in dev mode with the parachain template runtime. That runtime has the standard `System`, `Balances` and `Sudo` pallets, and `//Alice` holds funds. One process produces a block every 6 seconds, and every block is final at once. The script pins the node to the `polkadot-stable2609-1` release and the runtime to the template's `v0.0.5` release, and checks both downloads against SHA-256 sums.

You need Linux on x86-64 or macOS on Apple silicon, `bash`, `curl`, PolkaMeter and the substrate plugin. [Install](install.md) covers PolkaMeter. From v0.3.0, the install script and the container also install `polkameter-substrate-plugin`. From a checkout, build it with `cargo build -p polkameter-substrate-plugin`.

Run every command below from the root of a PolkaMeter checkout.

## Start the chain

```sh
scripts/dev-chain.sh start
```

```text
dev-chain: downloading https://github.com/paritytech/polkadot-sdk/releases/download/polkadot-stable2609-1/polkadot-omni-node-aarch64-apple-darwin
dev-chain: downloading https://github.com/paritytech/polkadot-sdk-parachain-template/releases/download/v0.0.5/parachain_template_runtime.compact.compressed.wasm
dev-chain: started pid 3216, log target/dev-chain/node.log
dev-chain: ready at ws://127.0.0.1:9944, block 21, 6000 ms blocks
```

The first start downloads about 155 MB into `target/dev-chain/bin`. Later starts reuse the files when their checksums match. `start` returns once the chain has 21 blocks, about two minutes, because preflight measures the block time over 20 blocks.

> **Warning:** `start` wipes the chain's data from the previous start. Each start is a new chain.

## Install the plugin and the seed

```sh
polkameter plugin install target/debug/polkameter-substrate-plugin
polkameter plugin credential substrate-dev POLKAMETER_SUBSTRATE_SEED
export POLKAMETER_SUBSTRATE_SEED='//Alice'
```

```text
Installed plugin substrate 0.3.0 (5 operations) from ~/polkameter/target/debug/polkameter-substrate-plugin
{
  "configured": true
}
```

The example plans name the credential profile `substrate-dev`. The plugin derives its signers from the seed, `//Alice` here, and funds them from it.

## Run the remarks plan

[`examples/substrate-remarks.polkameter.xml`](https://github.com/agustinustheo/polkameter/blob/main/examples/substrate-remarks.polkameter.xml) funds 16 flood signers and a probe signer, signs 65,000 remarks, and ramps from 100 to 600 transactions per second in steps of 30 seconds:

```sh
polkameter run examples/substrate-remarks.polkameter.xml --output target/runs --fail-on-verdict
echo $?
```

```text
Phase: preflight
Phase: setup
Phase: measurement
Phase: baseline
baseline: 3 probes landed, p50 6.0 s, max 6.0 s
Phase: load
step 0 prepared: target 100 tx/s, sent 100 tx/s, 4 blocks, 79.5 tx/s included, reply p95 0.0 s, 2 replies pending, backlog 616
...
step 4 prepared: target 500 tx/s, sent 500 tx/s, 5 blocks, 397.6 tx/s included, reply p95 0.0 s, 10 replies pending, backlog 3070
step 5 prepared: target 600 tx/s, sent 600 tx/s, 5 blocks, 407.6 tx/s included, reply p95 0.1 s, 12 replies pending, backlog 5771
stop: rate cap: all 6 steps ran
Phase: recovery
recovery: load stopped with 5771 txs outstanding; one probe per block for up to 120 s
recovery: back (3 probes in a row landed within 12.0 s); backlog 0 left
Phase: reconciliation
...
#### Result

- **Verdict:** FAIL
- **Failure onset** (first failed load step): step 5 (600 tx/s): latency, p95 send -> best block 16.7 s
- **Max sustained:** 500 tx/s target (step 4), 500 tx/s included
- **Load stop:** rate cap: all 6 steps ran
- **Recovery:** back 3 s after the load stopped (3 probes in a row landed within 12.0 s). Backlog at stop 5771 txs, drained after 8 s, at 320.5 tx/s.
- **Loss check:** 63001 sent: 63001 finalized (0 failed after inclusion), 0 refused, 0 still ready in the pool, 0 seen in a block but not verified final, **0 lost**, 0 unknown. ... State check: 100 of 100 every sampled transaction's signer nonce is past it at block 0x05e2cf0ac0cbd0305415364575a6a2225ec70f0f7c91f7bf688476faf5f4e8d3
...
#### Load steps

| step | target tx/s | duration s | transactions sent | sent/s | included/s | included % | p50 s | p95 s (limit 10.0) | ... | max ours/block | max ref time % | max proof % |
...
| 3 | 400 | 30.0 | 12000 | 400 | 400 | 100.0 | 5.0 | 7.7 | ... | 2406 | 78.2 | 5.4 |
| 4 | 500 | 30.0 | 15000 | 500 | 500 | 100.0 | 5.3 | 8.2 | ... | 3008 | 97.8 | 6.7 |
| 5 | 600 | 30.0 | 18001 | 600 | 600 | 100.0 | 7.0 | 16.7 | ... | 3075 | 100.0 | 6.9 |
...
Run completed_with_failures. Artifacts: ~/polkameter/target/runs/run-1793938251606-22106-1
1
```

The verdict is `FAIL` because the last step broke a limit. At 600 transactions per second, blocks were full: 3,075 remarks used 100% of a block's compute time (ref time). Transactions queued, and the p95 time from send to a block rose to 16.7 s, above the 10-second limit. The chain sustained 500 transactions per second. All 63,001 transactions sent were finalized, none were lost, and the state check found the nonce of every sampled transaction used. [Read the results](reading-results.md) explains each line of `summary.md`.

`--fail-on-verdict` makes a `FAIL` verdict exit with `1`. Leave it out when you explore a chain's limit, and keep it in CI.

## Run the transfers plan

[`examples/substrate-transfers.polkameter.xml`](https://github.com/agustinustheo/polkameter/blob/main/examples/substrate-transfers.polkameter.xml) loads `Balances.transfer_keep_alive` the same way, ramping from 80 to 480 transactions per second:

```sh
polkameter run examples/substrate-transfers.polkameter.xml --output target/runs --fail-on-verdict
```

```text
...
step 4 prepared: target 400 tx/s, sent 400 tx/s, 5 blocks, 307.0 tx/s included, reply p95 0.0 s, 8 replies pending, backlog 2789
step 5 prepared: target 480 tx/s, sent 480 tx/s, 5 blocks, 291.8 tx/s included, reply p95 0.0 s, 10 replies pending, backlog 5645
stop: rate cap: all 6 steps ran
...
- **Verdict:** FAIL
- **Failure onset** (first failed load step): step 5 (480 tx/s): latency, p95 send -> best block 18.6 s
- **Max sustained:** 400 tx/s target (step 4), 400 tx/s included
- **Load stop:** rate cap: all 6 steps ran
- **Recovery:** back 1 s after the load stopped (3 probes in a row landed within 12.0 s). Backlog at stop 5645 txs, drained after 13 s, at 313.6 tx/s.
- **Loss check:** 50401 sent: 50401 finalized (0 failed after inclusion), 0 refused, 0 still ready in the pool, 0 seen in a block but not verified final, **0 lost**, 0 unknown. ...
...
| 4 | 400 | 30.0 | 12000 | 400 | 400 | 100.0 | 4.6 | 7.3 | ... | 2309 | 100.0 | 4.3 |
| 5 | 480 | 30.0 | 14401 | 480 | 480 | 100.0 | 7.4 | 18.6 | ... | 2309 | 100.0 | 4.3 |
...
Run completed_with_failures. Artifacts: ~/polkameter/target/runs/run-1793938491211-22925-1
```

A transfer weighs more than a remark, so a block holds fewer: 2,309 transfers filled a block's compute time. The chain kept up at 400 transactions per second. At 480, transactions queued and the p95 time to a block rose to 18.6 s. All 50,401 transfers were finalized.

## Generate a plan for your chain

`polkameter init` writes a plan for the chain at an endpoint. It checks the runtime's calls and measures the block time:

```sh
polkameter init --rpc ws://127.0.0.1:9944 --profile substrate-dev --output target/init.polkameter.xml
polkameter validate target/init.polkameter.xml
```

```text
Wrote target/init.polkameter.xml for Custom (parachain-template-runtime 1, 6 s blocks). Next: polkameter validate target/init.polkameter.xml
Plan is structurally valid. Preflight checks the installed plugins.
```

The plan ramps from 10 to 50 transactions per second. Run it like the examples:

```sh
polkameter run target/init.polkameter.xml --output target/runs
```

```text
...
step 4 prepared: target 50 tx/s, sent 50 tx/s, 5 blocks, 39.8 tx/s included, reply p95 0.0 s, 1 replies pending, backlog 305
...
- **Verdict:** PASS
- **Failure onset** (first failed load step): no measure violated
- **Max sustained:** 50 tx/s target (step 4), 50 tx/s included
...
Run completed. Artifacts: ~/polkameter/target/runs/run-1793938851237-30197-1
```

`PASS` means that every step stayed within the limits, so the run did not find the chain's limit. Raise the ramp's rates and the prepare step's `count` until a step fails, as the remarks plan does.

## Load your own runtime

Set `POLKAMETER_DEV_RUNTIME` to your runtime's compressed Wasm to run it instead of the template:

```sh
POLKAMETER_DEV_RUNTIME=<path-to-your-runtime>.compact.compressed.wasm scripts/dev-chain.sh start
```

The runtime must run on `polkadot-omni-node`: a parachain runtime with Aura, as the parachain template builds. It must have a genesis preset named `development`, or the one you name in `POLKAMETER_DEV_PRESET`, that funds `//Alice`. Its block time is the runtime's slot duration, so set the plan's `block-interval-seconds` to match.

## Stop the chain

```sh
scripts/dev-chain.sh stop
```

```text
dev-chain: stopped pid 36098
```

`stop` stops only the node that `start` started.

## Script settings

| Variable | Default | Meaning |
| --- | --- | --- |
| `POLKAMETER_DEV_CHAIN_DIR` | `target/dev-chain` | Where the binaries, chain spec, chain data and `node.log` go |
| `POLKAMETER_DEV_CHAIN_PORT` | `9944` | The JSON-RPC port, for `ws://` and `http://` |
| `POLKAMETER_DEV_BLOCK_MS` | `6000` | Block time in milliseconds. The template runtime's timestamps assume 6000 |
| `POLKAMETER_DEV_WAIT_BLOCKS` | `21` | Blocks `start` waits for |
| `POLKAMETER_DEV_POOL_TYPE` | `single-state` | The transaction pool. See below |
| `POLKAMETER_DEV_RUNTIME` | the template runtime | Your runtime's Wasm |
| `POLKAMETER_DEV_PRESET` | `development` | The genesis preset of your runtime |
| `POLKAMETER_DEV_NODE_ARGS` | none | More `polkadot-omni-node` arguments, separated by spaces |

The script uses the single-state transaction pool. This node release's default, the fork-aware pool, loses transactions in dev mode. With `POLKAMETER_DEV_POOL_TYPE=fork-aware`, the remarks plan above finalized 2,826 of 62,993 transactions: from the 200 transactions per second step on, the pool accepted transactions without an error but almost no block included them. PolkaMeter reported them as lost. That loss comes from dev mode, not from the runtime, so the script avoids it.

## Run it in GitHub Actions

This job starts the dev chain, runs the short CI plans with `--fail-on-verdict`, and uploads the run directories. It caches the downloads on the script's hash. It needs v0.3.0 or newer, whose release archives hold the plugin. Save it as `.github/workflows/load-test.yml` in your repository, with `dev-chain.sh` copied from `scripts/` and the plan copied from `examples/`:

```yaml
name: load-test
on:
  pull_request:
  workflow_dispatch:

permissions:
  contents: read

jobs:
  load-test:
    runs-on: ubuntu-24.04
    timeout-minutes: 30
    env:
      POLKAMETER_DEV_CHAIN_DIR: ${{ github.workspace }}/.dev-chain
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
      - uses: actions/cache@55cc8345863c7cc4c66a329aec7e433d2d1c52a9 # v6.1.0
        with:
          path: .dev-chain/bin
          key: dev-chain-${{ runner.os }}-${{ hashFiles('dev-chain.sh') }}
      - name: Start the dev chain
        run: bash dev-chain.sh start
      - uses: agustinustheo/polkameter@v0
        with:
          plan: plans/substrate-remarks-ci.polkameter.xml
          plugins: polkameter-substrate-plugin
          credentials: substrate-dev=POLKAMETER_SUBSTRATE_SEED
          fail-on: verdict
        env:
          POLKAMETER_SUBSTRATE_SEED: //Alice
      - name: Stop the dev chain
        if: always()
        run: bash dev-chain.sh stop
```

`plugins: polkameter-substrate-plugin` names the plugin installed with the CLI. [Run PolkaMeter in CI](ci.md) lists the action's inputs. The PolkaMeter repository runs the same steps from source in its `dev-chain` CI job.

## Run it in GitLab CI

GitLab needs a Linux x86-64 runner. Add this job to `.gitlab-ci.yml`, with `dev-chain.sh` and the plan in your repository:

```yaml
load-test:
  image: debian:bookworm-slim
  variables:
    POLKAMETER_VERSION: "0.3.0"
    POLKAMETER_DEV_CHAIN_DIR: "$CI_PROJECT_DIR/.dev-chain"
    POLKAMETER_PLUGIN_REGISTRY: /tmp/polkameter-plugins.json
    POLKAMETER_SUBSTRATE_SEED: //Alice
  cache:
    key:
      files: [dev-chain.sh]
    paths: [.dev-chain/bin]
  before_script:
    - apt-get update && apt-get install -y --no-install-recommends ca-certificates curl
    - curl -fsSL https://raw.githubusercontent.com/agustinustheo/polkameter/main/install.sh | sh -s -- "$POLKAMETER_VERSION"
    - export PATH="$HOME/.local/bin:$PATH"
  script:
    - bash dev-chain.sh start
    - polkameter plugin install "$HOME/.local/bin/polkameter-substrate-plugin"
    - polkameter plugin credential substrate-dev POLKAMETER_SUBSTRATE_SEED
    - polkameter run plans/substrate-remarks-ci.polkameter.xml --output polkameter-runs --junit test-results/polkameter.xml --fail-on-verdict
  after_script:
    - bash dev-chain.sh stop
  artifacts:
    when: always
    reports:
      junit: test-results/polkameter.xml
    paths:
      - polkameter-runs/
      - test-results/polkameter.xml
```

`//Alice` is a public dev key, so it can sit in the job. For a shared network, put the seed in a masked CI/CD variable instead.
