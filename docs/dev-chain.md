# Stress-test a local dev chain

Let's start a dev chain and increase the transaction rate until a test limit fails. We'll use the [substrate plugin](substrate-plugin.md), so you don't write any plugin code. The same steps run in CI at the end.

The chain is `polkadot-omni-node` in dev mode with the parachain template runtime. That runtime has the standard `System`, `Balances` and `Sudo` pallets, and `//Alice` holds funds. It makes a block every 6 seconds, and each block is final at once. The script pins the node and runtime versions and checks both downloads against SHA-256 sums.

You need Linux on x86-64 or macOS on Apple silicon, with `bash` and `curl`. Clone [the repository](https://github.com/agustinustheo/polkameter), then build the CLI and the plugin from its root:

```sh
cargo build -p polkameter -p polkameter-substrate-plugin
export PATH="$PWD/target/debug:$PATH"
```

Run every command below from the root of the checkout.

## Start the chain

```sh
scripts/dev-chain.sh start
```

```text
dev-chain: downloading https://github.com/paritytech/polkadot-sdk/releases/download/polkadot-stable2609-1/polkadot-omni-node-aarch64-apple-darwin
dev-chain: downloading https://github.com/paritytech/polkadot-sdk-parachain-template/releases/download/v0.0.5/parachain_template_runtime.compact.compressed.wasm
dev-chain: started pid 38403, log .polkameter/dev-chain/node.log
dev-chain: ready at ws://127.0.0.1:9944, block 21, 6000 ms blocks
```

The first start downloads about 155 MB into `.polkameter/dev-chain/bin`. Add `.polkameter/` to your `.gitignore`. Later starts reuse the files.

`start` waits for 21 blocks, about two minutes. Preflight needs 20 blocks to measure the block time.

> **Warning:** `start` deletes the chain data from the previous start. Each start is a new chain.

## Install the plugin and the seed

```sh
polkameter plugin install target/debug/polkameter-substrate-plugin
polkameter plugin credential substrate-dev POLKAMETER_SUBSTRATE_SEED
export POLKAMETER_SUBSTRATE_SEED='//Alice'
```

```text
Installed plugin substrate 0.3.0 (5 operations) from ~/polkameter/target/debug/polkameter-substrate-plugin
Credential profile substrate-dev reads POLKAMETER_SUBSTRATE_SEED.
```

The second command maps the credential profile `substrate-dev` to the variable `POLKAMETER_SUBSTRATE_SEED`. The example plans use that profile. The plugin derives its signers from the seed, `//Alice` here, and funds them from it.

## Run the remarks plan

[`examples/substrate-remarks.polkameter.xml`](https://github.com/agustinustheo/polkameter/blob/main/examples/substrate-remarks.polkameter.xml) funds 16 flood signers and one signer per probe. It signs 65,000 remarks. Then it ramps from 100 to 600 transactions per second (tx/s), 30 seconds per step:

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
step 0 prepared: target 100 tx/s, sent 100 tx/s, 4 blocks, 79.8 tx/s included, reply p95 0.0 s, 2 replies pending, backlog 606
...
step 5 prepared: target 600 tx/s, sent 600 tx/s, 5 blocks, 409.6 tx/s included, reply p95 0.0 s, 12 replies pending, backlog 5712
stop: rate cap: all 6 steps ran
Phase: recovery
...
#### Result

- **Verdict:** FAIL
- **Failure onset** (first failed load step): step 5 (600 tx/s): latency, p95 send -> best block 12.9 s
- **Max sustained:** 500 tx/s target (step 4), 500 tx/s included
- **Load stop:** rate cap: all 6 steps ran
- **Recovery:** back 1 s after the load stopped (3 probes in a row landed within 12.0 s). Backlog at stop 5712 txs, drained after 8 s, at 317.3 tx/s.
- **Loss check:** 63002 sent: 63002 finalized (0 failed after inclusion), 0 refused, 0 still ready in the pool, 0 seen in a block but not verified final, **0 lost**, 0 unknown. ...
...
Run completed_with_failures. Artifacts: ~/polkameter/target/runs/run-1793952055743-80142-1
PolkaMeter: verdict FAIL: step 5 broke its latency measure (p95 send -> best block 12.9 s); exit 1 because of --fail-on-verdict (the checks are in summary.md)
1
```

The last step exceeded the latency limit, so the verdict is FAIL. At 600 tx/s, blocks were full. Transactions queued, and the p95 time from send to a block rose to 12.9 s. The limit is 10 s. "Failure onset" in the report is the breaking point. The max sustained rate was 500 tx/s. All 63,002 flood transactions were finalized, and none were lost. [Read the results](reading-results.md) explains each line.

`--fail-on-verdict` makes a FAIL verdict exit `1`. Leave it out while you explore, and keep it in CI.

[`examples/substrate-transfers.polkameter.xml`](https://github.com/agustinustheo/polkameter/blob/main/examples/substrate-transfers.polkameter.xml) does the same with `Balances.transfer_keep_alive`. A transfer weighs more than a remark. On this chain it kept up at 400 tx/s and broke the latency limit at 480.

## Generate a plan for your chain

`polkameter init` writes a plan for the chain at an endpoint. It checks the runtime's calls, measures the block time, and sets the transactions' mortality to cover the run:

```sh
polkameter init --rpc ws://127.0.0.1:9944 --profile substrate-dev --output target/init.polkameter.xml
polkameter validate target/init.polkameter.xml
```

```text
Wrote target/init.polkameter.xml for Custom (parachain-template-runtime 1, 6 s blocks). Next: polkameter validate target/init.polkameter.xml
Plan is structurally valid. Preflight checks the installed plugins.
```

The plan ramps from 10 to 50 tx/s. Run it like the examples:

```sh
polkameter run target/init.polkameter.xml --output target/runs
```

```text
...
- **Verdict:** PASS
- **Failure onset** (first failed load step): no measure violated
- **Max sustained:** 50 tx/s target (step 4), 50 tx/s included
...
Run completed. Artifacts: ~/polkameter/target/runs/run-1793938851237-30197-1
```

PASS means every step stayed within the limits. So this run didn't find the chain's limit. Raise the ramp's rates and the prepare step's `count` until a step fails, as the remarks plan does.

## Load your own runtime

Set `POLKAMETER_DEV_RUNTIME` to your runtime's compressed Wasm to run it instead of the template:

```sh
POLKAMETER_DEV_RUNTIME=<path-to-your-runtime>.compact.compressed.wasm scripts/dev-chain.sh start
```

The runtime must run on `polkadot-omni-node`. That means a parachain runtime with Aura, like the parachain template. It needs a genesis preset that funds `//Alice`, named `development` or the name in `POLKAMETER_DEV_PRESET`. `start` doesn't know its block time. Set the plan's `block-interval-seconds` to the runtime's slot duration, or let `polkameter init` measure it.

## Stop the chain

```sh
scripts/dev-chain.sh stop
```

```text
dev-chain: stopped pid 38403
```

`stop` stops only the node that `start` started.

## Script settings

The header of [`scripts/dev-chain.sh`](https://github.com/agustinustheo/polkameter/blob/main/scripts/dev-chain.sh) lists its variables. You'll most likely need `POLKAMETER_DEV_CHAIN_PORT` (default `9944`) when the port is taken, and `POLKAMETER_DEV_CHAIN_DIR` (default `.polkameter/dev-chain`) to move the files.

The script uses the single-state transaction pool. The node's default fork-aware pool loses transactions in dev mode, and PolkaMeter would report them as lost.

## Run it in GitHub Actions

This job starts the dev chain, runs the short CI plan with `fail-on: verdict`, and uploads the run's reports. It caches the downloads on the script's hash. Commit these files to your repository:

- the job as `.github/workflows/load-test.yml`
- `dev-chain.sh` at the repository root, from the same PolkaMeter version
- the CI plan as `plans/substrate-remarks-ci.polkameter.xml`

```sh
curl -fsSLO https://raw.githubusercontent.com/agustinustheo/polkameter/v0.3.0/scripts/dev-chain.sh
mkdir -p plans
curl -fsSL -o plans/substrate-remarks-ci.polkameter.xml https://raw.githubusercontent.com/agustinustheo/polkameter/v0.3.0/examples/substrate-remarks-ci.polkameter.xml
```

Keep the action's `version` equal to the plan's substrate plugin version, `<plugin id="substrate" version="0.3.0">`. The plugin ships with the CLI, and PolkaMeter refuses a plugin of another version.

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
    steps:
      - uses: actions/checkout@3d3c42e5aac5ba805825da76410c181273ba90b1 # v7.0.1
      # The node and runtime are pinned in dev-chain.sh; cache them on its hash.
      - uses: actions/cache@55cc8345863c7cc4c66a329aec7e433d2d1c52a9 # v6.1.0
        with:
          path: .polkameter/dev-chain/bin
          key: dev-chain-${{ runner.os }}-${{ hashFiles('dev-chain.sh') }}
      - name: Start the dev chain
        run: bash dev-chain.sh start
      # To test your own runtime instead of the template, build its wasm first and set
      # POLKAMETER_DEV_RUNTIME to its path on the step above.
      - id: load
        # @v0 is the action; version pins the CLI and the substrate plugin it installs.
        uses: agustinustheo/polkameter@v0
        with:
          version: 0.3.0
          plan: plans/substrate-remarks-ci.polkameter.xml
          plugins: polkameter-substrate-plugin
          credentials: substrate-dev=POLKAMETER_SUBSTRATE_SEED
          fail-on: verdict
        env:
          # //Alice is funded on the dev chain. On a shared network, use a secret instead.
          POLKAMETER_SUBSTRATE_SEED: //Alice
      - name: Stop the dev chain
        if: always()
        run: bash dev-chain.sh stop
```

`plugins: polkameter-substrate-plugin` names the plugin installed with the CLI. [Run in CI](ci.md) covers the action.

## Run it in GitLab CI

Use a Linux x86-64 runner. Add this job to `.gitlab-ci.yml`, and commit `dev-chain.sh` and the plan as for GitHub. Keep `POLKAMETER_VERSION` equal to the plan's substrate plugin version:

```yaml
load-test:
  image: debian:bookworm-slim
  variables:
    POLKAMETER_VERSION: "0.3.0"
    POLKAMETER_PLUGIN_REGISTRY: /tmp/polkameter-plugins.json
    # //Alice is funded on the dev chain. On a shared network, use a masked CI/CD variable.
    POLKAMETER_SUBSTRATE_SEED: //Alice
  cache:
    key:
      files: [dev-chain.sh]
    paths: [.polkameter/dev-chain/bin]
  before_script:
    - apt-get update && apt-get install -y --no-install-recommends ca-certificates curl
    - curl -fsSL https://raw.githubusercontent.com/agustinustheo/polkameter/main/install.sh | sh -s -- "$POLKAMETER_VERSION"
    - export PATH="$HOME/.local/bin:$PATH"
  script:
    - bash dev-chain.sh start
    - polkameter plugin install "$HOME/.local/bin/polkameter-substrate-plugin"
    - polkameter plugin credential substrate-dev POLKAMETER_SUBSTRATE_SEED
    - polkameter validate plans/substrate-remarks-ci.polkameter.xml
    - polkameter run plans/substrate-remarks-ci.polkameter.xml --output polkameter-runs --fail-on-verdict
  after_script:
    - bash dev-chain.sh stop
  artifacts:
    when: always
    reports:
      junit: polkameter-runs/run-*/junit.xml
    paths:
      - polkameter-runs/
```

`//Alice` is a public dev key, so it's fine in the job. On a shared network, put the seed in a masked CI/CD variable.

You now have a chain, a failing step and a job that catches it. Have fun breaking things!
