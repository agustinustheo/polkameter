# Run a load test

A load test sends transactions to a chain at rates you set, then measures how the chain copes. PolkaMeter does not build transactions for your runtime, and this repository ships no plugin that does. You need three things:

1. A node that answers on `ws://` or `wss://`.
2. A plugin that prepares the flood and probe transactions for your runtime. [Writing a plugin](writing-a-plugin.md) shows how to write one. The People plugin in [paritytech/polkadot-pop-e2e](https://github.com/paritytech/polkadot-pop-e2e/tree/feat/th-polkameter-stress-flood/stress) is a real example.
3. A plan with a `<targets>` element and a `<load>` element.

## Write a plan

This skeleton validates, but it cannot run yet. `my-runtime` and its operation names are placeholders. Your plugin defines the real names and outputs.

```xml
<polkameter-plan xmlns="https://polkameter.dev/schema/plan" version="1" name="First load" timeout-ms="3600000">
  <plugins><plugin id="my-runtime" version="0.1.0" protocol="1"/></plugins>
  <targets><target id="chain" endpoint="ws://127.0.0.1:9944"/></targets>
  <setup>
    <step id="flood" use="my-runtime.prepare-flood"><input name="count" value="1000"/></step>
    <step id="probes" use="my-runtime.prepare-probes"><input name="count" value="50"/></step>
  </setup>
  <load target="chain" source-ref="steps.flood.transactions" probes-ref="steps.probes.transactions" block-interval-seconds="6">
    <rate tx-per-second="2" seconds="30"/>
  </load>
</polkameter-plan>
```

- `<targets>` names the node. The `<load target>` must refer to a target with a `ws://` or `wss://` endpoint.

- `source-ref` is the flood, and `probes-ref` is the list reserved for baseline and recovery probes. The two lists must not overlap.
- `block-interval-seconds` is the block time you expect. Preflight compares it with the chain.

> **Warning:** A load test sends many transactions to its endpoint. Point a plan only at a node you run, or one you have permission to load. Do not point a plan at a public network without permission from its operators.

Choose one load profile:

| Profile | Element | How the rate changes |
| --- | --- | --- |
| Rate steps | One or more `<rate tx-per-second="N" seconds="S"/>` | Each step runs at a fixed rate for `seconds` |
| Ramp | One `<ramp start="N" step="N" steps="N" seconds="S"/>` | Starts at `start` and adds `step` (or multiplies by `growth`) at each of `steps` steps. `<ramp start="2" step="2" steps="3" seconds="30"/>` runs at 2, 4 and 6 tx/s |

Rate steps and a ramp cannot be combined. Use rate steps for rates you choose, and a ramp to find where the chain starts to struggle. The [plan reference](plans.md#load) lists every attribute and limit.

## Run the test

Run these from the directory that holds your plan. Put `polkameter` on your `PATH` first, as [Getting started](getting-started.md) shows.

1. Install your plugin. Install it again after every rebuild, because PolkaMeter refuses a plugin whose file has changed since install.

   ```sh
   polkameter plugin install <path-to-plugin-executable>
   polkameter plugin topology <alias> <path-to-zombie.json>   # only if the plan has monitors
   polkameter plugin credential <profile> <ENV_VAR>            # only if the plan has credentials
   ```

2. Check the plan and the chain.

   ```sh
   polkameter validate plan.polkameter.xml
   polkameter preflight plan.polkameter.xml
   ```

   `validate` checks the plan's structure without starting plugins or contacting the chain. `preflight` starts the plugins, checks each operation against its manifest and the evidence the plugins need, and runs the plan's read-only preflight steps. For a load plan, it also measures the block interval. If the measured value differs from `block-interval-seconds` by more than 25%, preflight stops before setup and exits with code `3`.

3. Run the plan.

   ```sh
   polkameter run plan.polkameter.xml --output runs
   ```

   The run creates one directory inside `runs/`, named `run-<milliseconds>-<pid>-<sequence>`. Ctrl-C stops a local run. The run writes what it has and exits with code `130`.

Open `summary.md` in the run directory first. [Read the results](reading-results.md) explains it. [Run directory](results.md#the-run-directory) lists every file.

## On a local Zombienet network

`scripts/local-fork-run.sh` runs a plan on a fresh local Zombienet network. It starts the network from a TOML file, waits for blocks, installs your plugins and credentials, registers the plan's topology, runs the plan and stops the network. Run it from the repository root.

```text
scripts/local-fork-run.sh: line 20: 1: usage: scripts/local-fork-run.sh NETWORK_TOML PLAN [OUTPUT_DIR]
```

That is the usage line, printed when you run the script with no arguments.

You need:

- `bash` and `python3`. The script polls the nodes with `python3`.
- A release build of the CLI: `cargo +1.93.0 build --release -p polkameter --bin polkameter`, or set `POLKAMETER` to another binary.
- Built plugin executables for `POLKAMETER_PLUGINS`.
- `zombie-cli` on `PATH`, or next to the binary named by `default_command` in the TOML.
- The node binaries that your Zombienet TOML starts. For a Previewnet fork, `ppn fork toml <bundle> <out>` creates the TOML, as the script's header notes.
- Free local ports for every `ws://` endpoint in the plan. The script refuses to start if one is in use.

| Variable | Default | Meaning |
| --- | --- | --- |
| `POLKAMETER_PLUGINS` | none | Plugin executables to install, separated by spaces |
| `POLKAMETER_CREDENTIALS` | none | Credential profiles as `profile=ENV_VAR`, separated by spaces. The variable must be exported in the shell that runs the script |
| `POLKAMETER` | `target/release/polkameter` | The CLI to run |
| `ZOMBIE_CLI` | `zombie-cli` next to the TOML's `default_command` binary, or on `PATH` | The `zombie-cli` binary |
| `PPN_BIN_DIR` | The directory of the TOML's `default_command` binary | Where the Previewnet omni-node wrapper finds the collator |

Example:

```sh
POLKAMETER_PLUGINS="target/release/my-runtime-plugin" \
POLKAMETER_CREDENTIALS="my-profile=MY_SETUP_SURI" \
scripts/local-fork-run.sh ~/previewnet/zombienet.toml plans/first-load.polkameter.xml target/local-runs/first
```

The plan's `ws://` ports must match the TOML. The script waits up to 15 minutes per endpoint for blocks. Results go to `<OUTPUT_DIR>/results`, the network's files to `<OUTPUT_DIR>/network`, and its log to `<OUTPUT_DIR>/network.log`. The script stops the network on exit and returns the run's exit code.
