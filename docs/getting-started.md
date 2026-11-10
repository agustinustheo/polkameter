# Getting started

Let's start a local dev chain and raise the transaction rate until the chain fails one of the test's limits. We'll use the [substrate plugin](substrate-plugin.md), so you don't write any plugin code.

The dev chain is one `polkadot-omni-node` in dev mode, running the parachain template runtime. It makes a block every 6 seconds. Each block is finalized at once, so the chain never replaces it. `//Alice` has funds.

You need `bash` and `curl`. The dev chain script pins node builds for Linux x86-64 and macOS on Apple silicon only. Run every command below in the same directory, such as your chain's repository.

## 1. Install

```sh
curl -fsSL https://raw.githubusercontent.com/agustinustheo/polkameter/main/install.sh | sh -s -- 0.4.6
export PATH="$HOME/.local/bin:$PATH"
polkameter --version
```

Every command on this page uses release 0.4.6, as [Versions](install.md#versions) asks. [Install](install.md) has other methods.

## 2. Start a dev chain

```sh
curl -fsSLO https://raw.githubusercontent.com/agustinustheo/polkameter/v0.4.6/scripts/dev-chain.sh
bash dev-chain.sh start
```

```text
dev-chain: downloading https://github.com/paritytech/polkadot-sdk/releases/download/polkadot-stable2609-1/polkadot-omni-node-aarch64-apple-darwin
dev-chain: downloading https://github.com/paritytech/polkadot-sdk-parachain-template/releases/download/v0.0.5/parachain_template_runtime.compact.compressed.wasm
dev-chain: started pid 38403, log .polkameter/dev-chain/node.log
dev-chain: ready at ws://127.0.0.1:9944, block 21, 6000 ms blocks
```

The first start downloads the node and runtime, about 155 MB, into `.polkameter/dev-chain/bin` and checks their SHA-256 sums. Add `.polkameter/` to your `.gitignore`. Later starts reuse the downloads, but each start is a new chain: `start` deletes the previous chain data.

`start` waits for 21 blocks, about two minutes. PolkaMeter's preflight checks measure the block time over 20 blocks, so the chain needs them first.

## 3. Register the plugin and the seed

```sh
polkameter plugin install "$(command -v polkameter-substrate-plugin)"
polkameter plugin credential substrate-dev POLKAMETER_SUBSTRATE_SEED
export POLKAMETER_SUBSTRATE_SEED='//Alice'
```

```text
Installed plugin substrate 0.4.6 (5 operations) from ~/.local/bin/polkameter-substrate-plugin
Credential profile substrate-dev reads POLKAMETER_SUBSTRATE_SEED.
```

A plan never holds a secret. It names a [credential profile](cli.md#plugin), here `substrate-dev`. A profile is a name for an environment variable on your machine. The second command points `substrate-dev` at the variable that holds the seed.

## 4. Run a load test

A load test sends two kinds of [transactions](plans.md#load). The flood is the main load. PolkaMeter sends it at the target rate. A probe is a single transaction that measures how long it takes to get into a block. Probes go before and after the flood.

[`examples/substrate-remarks.polkameter.xml`](https://github.com/agustinustheo/polkameter/blob/v0.4.6/examples/substrate-remarks.polkameter.xml) funds 16 flood accounts and one account per probe. It signs 65,000 `System.remark` transactions. Then it sends about 63,000 of them in six [rate steps](plans.md#rate) of 30 seconds, from 100 to 600 transactions per second (tx/s). The rest are spare:

```sh
mkdir -p plans
curl -fsSL -o plans/substrate-remarks.polkameter.xml https://raw.githubusercontent.com/agustinustheo/polkameter/v0.4.6/examples/substrate-remarks.polkameter.xml
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

Rate steps count from 0. The FAIL and the exit code `1` are what we wanted: the last rate step pushed the chain past a limit. While you explore, add `--fail-on error`. Then the verdict never fails the command. Only an error in the run does.

Open `summary.md` in the run directory that the `Run …` line names:

```markdown
- **Verdict:** FAIL
- **Failure onset** (first failed load step): step 5 (600 tx/s): latency, p95 send -> best block 12.9 s
- **Max sustained:** 500 tx/s target (step 4), 500 tx/s included
- **Load stop:** rate cap: all 6 steps ran
```

The failure onset is the first rate step that failed a limit. In step 5, 95% of the transactions got into a block within 12.9 s, but the limit is 10 s. So step 4, at 500 tx/s, is the max sustained rate. [Read the results](reading-results.md) explains every line.

## 5. Generate a plan for your chain

`polkameter init` checks the chain at an endpoint and writes a plan for it. It also makes the signed transactions stay valid until the run ends:

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

`Custom` is the chain name the node reports. The plan's rate steps go from 10 to 50 tx/s. The dev chain keeps up with that. To find its limit, open `plans/init.polkameter.xml` and change the `<ramp>` element to this:

```xml
<ramp start="200" step="100" steps="5" seconds="30"/>
```

This ramp sends up to 60,000 transactions. The prepare step must sign more than that, so set its count to `<input name="count" value="65000"/>`. Then run it again:

```sh
polkameter run plans/init.polkameter.xml --output runs
```

[Generate a plan](substrate-plugin.md#generate-a-plan) says what to change for a longer run.

> **Warning:** A load test sends many transactions to its endpoint. Point a plan only at a node you run, or one you have permission to load.

## 6. Load your own runtime

Stop the dev chain (step 7). Then start it with your runtime's `*.compact.compressed.wasm`, which `cargo build --release` writes under `target/release/wbuild/`:

```sh
POLKAMETER_DEV_RUNTIME=<path-to-your-runtime>.compact.compressed.wasm bash dev-chain.sh start
```

The runtime must be a parachain runtime with Aura, like the parachain template, so that `polkadot-omni-node` can run it. Its genesis preset, the starting state, must fund `//Alice`. Name the preset `development`, or set `POLKAMETER_DEV_PRESET` to its name. Then generate and run a plan for it as in step 5.

## 7. Stop the chain

```sh
bash dev-chain.sh stop
```

```text
dev-chain: stopped pid 38403
```

`stop` stops only the node that `start` started.

Next, [run it in CI](ci.md). Have fun breaking things!
