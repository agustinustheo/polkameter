# Run a load test

A load test sends transactions to a chain at rates you choose. PolkaMeter measures how many reach a block and how long they take.

It uses two kinds of transactions. Flood transactions are the main load, sent at the target rate. Probe transactions are sent before and after the flood to time how long one transaction takes to reach a block.

You need three things:

1. A node that answers on `ws://` or `wss://`.
2. A plugin that prepares the flood and probe transactions for your runtime. If your chain has the standard `System` and `Balances` pallets, try the [substrate plugin](substrate-plugin.md). [Stress-test a local dev chain](dev-chain.md) runs it end to end. For other calls, see [Writing a plugin](writing-a-plugin.md).
3. A plan with a `<targets>` element and a `<load>` element.

## Write a plan

Replace `my-runtime` and the operation names with your plugin's names before you run this plan:

```xml
<polkameter-plan xmlns="https://polkameter.dev/schema/plan" version="1" name="First load" timeout-ms="3600000">
  <plugins><plugin id="my-runtime" version="0.1.0" protocol="1"/></plugins>
  <targets><target id="chain" endpoint="ws://127.0.0.1:9944"/></targets>
  <setup>
    <step id="flood" use="my-runtime.prepare-flood"><input name="count" value="1000"/></step>
    <step id="probes" use="my-runtime.prepare-probes"><input name="count" ref="run.probeBudget"/></step>
  </setup>
  <load target="chain" source-ref="steps.flood.transactions" probes-ref="steps.probes.transactions" block-interval-seconds="6">
    <rate tx-per-second="2" seconds="30"/>
  </load>
</polkameter-plan>
```

- `source-ref` points to the flood. `probes-ref` points to the probes.
- `ref="run.probeBudget"` asks the plugin for exactly as many probes as the run needs. Here that's 165. The [plan reference](plans.md#load) gives the formula.
- Use separate prepared transactions for the flood and the probes. Don't take probes from the flood list, for example use a separate signer.
- `block-interval-seconds` is the block time you expect. Preflight checks it against the chain.

Each `<rate>` is a rate step: a fixed target rate, in transactions per second (tx/s), for a set time. You can also use one `<ramp>` instead. `<ramp start="2" step="2" steps="3" seconds="30"/>` runs at 2, 4 and 6 tx/s. Use rate steps for rates you pick, and a ramp to find where the chain starts to struggle. See [rate](plans.md#rate) and [ramp](plans.md#ramp).

> **Warning:** A load test sends many transactions to its endpoint. Point a plan only at a node you run, or one you have permission to load.

## Run the test

Run these from the directory that holds your plan. Put `polkameter` on your `PATH` first, as [Getting started](getting-started.md) shows.

1. Install your plugin. Install it again after every rebuild, or PolkaMeter refuses it.

   ```sh
   polkameter plugin install <path-to-plugin-executable>
   polkameter plugin topology <alias> <path-to-zombie.json>   # only if the plan has monitors
   polkameter plugin credential <profile> <ENV_VAR>            # only if the plan has credentials
   ```

2. Check the plan, then the plugins and the chain.

   ```sh
   polkameter validate plan.polkameter.xml
   polkameter preflight plan.polkameter.xml
   ```

   `validate` doesn't start plugins or contact the chain. `preflight` does both. It checks each operation against the plugin's manifest, runs the plan's read-only preflight steps and measures the block time. Nothing is sent to the chain yet. See [preflight](cli.md#preflight).

3. Run the plan.

   ```sh
   polkameter run plan.polkameter.xml --output runs
   ```

   The run creates one directory inside `runs/`, named `run-<milliseconds>-<pid>-<sequence>`. Ctrl-C stops the run cleanly. See [Stop a run](cli.md#stop-a-run).

Open `summary.md` in the run directory first. [Read the results](reading-results.md) explains it.

To run a plan on a fresh local Zombienet network, use [`scripts/local-fork-run.sh`](https://github.com/agustinustheo/polkameter/blob/main/scripts/local-fork-run.sh). Its header lists what it needs.
