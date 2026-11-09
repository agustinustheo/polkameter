# Getting started

Let's start a dev chain and increase the transaction rate until a test limit fails. We'll use the [substrate plugin](substrate-plugin.md), so you don't write any plugin code.

The chain is `polkadot-omni-node` in dev mode with the parachain template runtime. That runtime has the standard `System`, `Balances` and `Sudo` pallets, and `//Alice` holds funds. It makes a block every 6 seconds, and each block is final at once. The script pins the node and runtime versions and checks both downloads against SHA-256 sums.

You need Linux on x86-64 or macOS on Apple silicon, with `bash` and `curl`.

## 1. Install

```sh
curl -fsSL https://raw.githubusercontent.com/agustinustheo/polkameter/main/install.sh | sh
export PATH="$HOME/.local/bin:$PATH"
polkameter --version
```

[Install](install.md) has other methods.

## 2. Start a dev chain

Fetch the dev chain script and start it:

```sh
curl -fsSLO https://raw.githubusercontent.com/agustinustheo/polkameter/v0.3.0/scripts/dev-chain.sh
bash dev-chain.sh start
```

```text
dev-chain: downloading https://github.com/paritytech/polkadot-sdk/releases/download/polkadot-stable2609-1/polkadot-omni-node-aarch64-apple-darwin
dev-chain: downloading https://github.com/paritytech/polkadot-sdk-parachain-template/releases/download/v0.0.5/parachain_template_runtime.compact.compressed.wasm
dev-chain: started pid 38403, log .polkameter/dev-chain/node.log
dev-chain: ready at ws://127.0.0.1:9944, block 21, 6000 ms blocks
```

The first start downloads about 155 MB into `.polkameter/dev-chain/bin`. Add `.polkameter/` to your `.gitignore`. Later starts reuse the files. If you set `POLKAMETER_DEV_CHAIN_PORT`, change the plan's endpoint and later `--rpc` too.

`start` waits for 21 blocks, about two minutes. Preflight needs 20 blocks to measure the block time.

> **Warning:** `start` deletes the chain data from the previous start. Each start is a new chain.

## 3. Install the plugin and the seed

```sh
polkameter plugin install "$(command -v polkameter-substrate-plugin)"
polkameter plugin credential substrate-dev POLKAMETER_SUBSTRATE_SEED
export POLKAMETER_SUBSTRATE_SEED='//Alice'
```

```text
Installed plugin substrate 0.3.0 (5 operations) from ~/.local/bin/polkameter-substrate-plugin
Credential profile substrate-dev reads POLKAMETER_SUBSTRATE_SEED.
```

The second command maps the credential profile `substrate-dev` to the variable `POLKAMETER_SUBSTRATE_SEED`. The example plans use that profile.

## 4. Run a stress plan

[`examples/substrate-remarks.polkameter.xml`](https://github.com/agustinustheo/polkameter/blob/v0.3.0/examples/substrate-remarks.polkameter.xml) funds 16 [flood](plans.md#load) signers and one signer per [probe](plans.md#load). It signs 65,000 remarks. Then it ramps from 100 to 600 transactions per second (tx/s), 30 seconds per step:

```sh
mkdir -p plans
curl -fsSL -o plans/substrate-remarks.polkameter.xml https://raw.githubusercontent.com/agustinustheo/polkameter/v0.3.0/examples/substrate-remarks.polkameter.xml
polkameter run plans/substrate-remarks.polkameter.xml --output runs
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
Run completed_with_failures. Artifacts: ~/runs/run-1793952055743-80142-1
PolkaMeter: verdict FAIL: step 5 broke its latency measure (p95 send -> best block 12.9 s); exit 1 ...
1
```

Open `summary.md` in the run directory:

```markdown
- **Verdict:** FAIL
- **Failure onset** (first failed load step): step 5 (600 tx/s): latency, p95 send -> best block 12.9 s
- **Max sustained:** 500 tx/s target (step 4), 500 tx/s included
- **Load stop:** rate cap: all 6 steps ran
```

The verdict is FAIL because step 5 broke the latency limit: its p95 time from send to a block was 12.9 s, and the limit is 10 s. The max sustained rate was 500 tx/s. No transaction was lost. [Read the results](reading-results.md) explains each line.

A FAIL verdict exits `1`, so CI catches it. While you explore, add `--fail-on error` to exit `1` only when the run itself fails.

## 5. Generate a plan for your chain

`polkameter init` writes a plan for the chain at an endpoint. It checks the runtime's calls, measures the block time, and sets the transactions' mortality to cover the run:

```sh
polkameter init --rpc ws://127.0.0.1:9944 --output plans/init.polkameter.xml
polkameter validate plans/init.polkameter.xml
polkameter preflight plans/init.polkameter.xml
```

```text
Wrote plans/init.polkameter.xml for Custom (parachain-template-runtime 1, 6 s blocks). Next: polkameter validate plans/init.polkameter.xml
Plan is structurally valid. Preflight checks the installed plugins.
Preflight passed.
```

The plan ramps from 10 to 50 tx/s, so a dev chain passes it. Raise the ramp's rates and the prepare step's `count` until a step fails, as the remarks plan does. [Generate a plan](substrate-plugin.md#generate-a-plan) says what else to change.

> **Warning:** A load test sends many transactions to its endpoint. Point a plan only at a node you run, or one you have permission to load.

## 6. Load your own runtime

Stop the dev chain, then set `POLKAMETER_DEV_RUNTIME` to your runtime's compressed Wasm:

```sh
POLKAMETER_DEV_RUNTIME=<path-to-your-runtime>.compact.compressed.wasm bash dev-chain.sh start
```

The runtime must run on `polkadot-omni-node`. That means a parachain runtime with Aura, like the parachain template. It needs a genesis preset that funds `//Alice`, named `development` or the name in `POLKAMETER_DEV_PRESET`. `start` doesn't know its block time. Set the plan's `block-interval-seconds` to the runtime's slot duration, or let `polkameter init` measure it.

## 7. Stop the chain

```sh
bash dev-chain.sh stop
```

```text
dev-chain: stopped pid 38403
```

`stop` stops only the node that `start` started.

Next, [run it in CI](ci.md). Have fun breaking things!
