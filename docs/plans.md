# Plan reference

A plan is an XML file that lists the test's steps and settings. It never holds a secret or a file path. Those stay on the machine that runs the plan. You set them with [`polkameter plugin`](cli.md#plugin).

## Example

This example has targets, credentials, a load test, monitors and thresholds. Its plugin, operations and endpoints are placeholders. It passes `validate`, but it cannot run until you replace them:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<!-- my-plugin, its operations and the endpoints are placeholders. Replace them with real values. -->
<polkameter-plan xmlns="https://polkameter.dev/schema/plan" version="1" name="Transfer load test" mode="stress" timeout-ms="7200000">
  <plugins>
    <plugin id="my-plugin" version="0.1.0" protocol="1"/>
  </plugins>
  <targets>
    <target id="relay" endpoint="ws://127.0.0.1:9944"/>
    <target id="para" endpoint="ws://127.0.0.1:9988"/>
  </targets>
  <credentials>
    <credential id="admin" profile="sudo"/>
  </credentials>
  <setup>
    <step id="prepare" use="my-plugin.prepare-transfers" timeout-ms="120000">
      <input name="target" ref="targets.para"/>
      <input name="signer" ref="credentials.admin"/>
      <input name="count" value="50000"/>
    </step>
  </setup>
  <load target="para" source-ref="steps.prepare.flood" probes-ref="steps.prepare.probes"
        state-check="my-plugin.check-state" state-ref="steps.prepare.expected"
        connections="8" baseline-probes="5" recovery-seconds="600" block-interval-seconds="12">
    <rate tx-per-second="20" seconds="60"/>
    <rate tx-per-second="40" seconds="60"/>
  </load>
  <monitors topology="test-network" relay-target="relay" para-id="2000">
    <metric role="collator" name="substrate_block_height"/>
  </monitors>
  <thresholds max-p95-latency-ms="8000" min-included-ratio="0.95"/>
</polkameter-plan>
```

`prepare` runs in setup, so the load can read its outputs `flood`, `probes` and `expected`. `<thresholds>` changes the inclusion and latency limits.

In the tables below, the Default column gives the default value, `required`, or the rule that makes the attribute required. An ID uses ASCII letters, digits, `-` and `_`. A reference is a value from [References](#references).

## Elements

### polkameter-plan

The root element. Its `name` appears in the report.

Children, in any order: `plugins`, `targets`, `credentials`, `preflight`, `setup`, `load`, `monitors`, `thresholds`, `evaluate`, `teardown`. Each appears at most once. Whatever their order in the XML, a run goes through these phases. `run` prints each as a `Phase:` line:

1. `preflight`: checks the installed plugins against the plan and runs the preflight steps. For a load test, it also compares the block interval with the chain's.
2. `setup`: setup steps run once, usually to prepare the load's transactions.
3. `measurement`, in a load test. It holds four phases: `baseline` (probes before the flood), `load` (the rate steps), `recovery`, and `reconciliation` (the wait for finality and the loss check).
4. Evaluate steps run, with no phase line of their own.
5. `checks`, in a load test: the outcome checks and the verdict.
6. `teardown`: teardown steps run even after a failure in setup or later. A run that fails in preflight sent nothing, so it skips teardown.

Steps in a phase run in their written order. Last, the run writes its reports.

| Attribute | Type | Default | Limits | Meaning |
| --- | --- | --- | --- | --- |
| `xmlns` | URI | required | `https://polkameter.dev/schema/plan` | The plan namespace |
| `name` | string | required | not empty | The test's title in the report |
| `version` | integer | required | `1` | The plan format version |
| `mode` | string | `stress` | `stress` or `smoke` | See below |
| `timeout-ms` | integer | `3600000` | above 0 | The run deadline. See below |

Stress mode records monitor problems, such as a node that stops answering, and goes on. Only a [stop rule](reading-results.md#stop-rules) ends the load early. Smoke mode is for a quick check that must catch any problem. It stops the load at the first monitor problem and has [stricter exit rules](cli.md#run).

`timeout-ms` limits everything before teardown. A run that reaches it fails and exits `1`, or `3` in preflight.

### plugins, targets, credentials

These elements hold zero or more `plugin`, `target` and `credential` elements. A step can call only operations of a declared plugin, or `core` operations.

### plugin

Declares one plugin. The version and protocol must match the installed plugin.

| Attribute | Type | Default | Limits | Meaning |
| --- | --- | --- | --- | --- |
| `id` | identifier | required | unique; not `core` | The prefix of the plugin's operations, such as `example.double` |
| `version` | string | required | not empty | The installed plugin's version |
| `protocol` | integer | required | `1` | The plugin protocol version |

### target

| Attribute | Type | Default | Limits | Meaning |
| --- | --- | --- | --- | --- |
| `id` | identifier | required | unique | Referenced as `targets.ID` |
| `endpoint` | URI | required | starts with `ws://` or `wss://` | The chain's WebSocket RPC endpoint. Secrets in it are [redacted](results.md#redaction) |

### credential

| Attribute | Type | Default | Limits | Meaning |
| --- | --- | --- | --- | --- |
| `id` | identifier | required | unique | Referenced as `credentials.ID` |
| `profile` | string | required | not empty | A [credential profile](cli.md#plugin) on the run host |

### preflight, setup, evaluate, teardown

Each holds zero or more `step` elements.

- `preflight` steps can use only read-only operations.
- An `evaluate` step whose operation returns `checks` adds them to the report. See [Observers and checks](plugins.md#observers-and-checks).
- `teardown` steps clean up, also after a failure or a stop. Every one runs, even if an earlier one fails.


### load

Makes the plan a load test against one target. A load test sends transactions at chosen rates and measures how the chain responds.

It sends two lists of prepared transactions, which must not share a transaction. Flood transactions are the main load, sent at the target rate. A probe transaction measures the time from submission to inclusion. PolkaMeter sends one probe per block: baseline probes before the flood, and recovery probes after it.

Children: `rate` (one or more) or one `ramp`. Give one or the other.

| Attribute | Type | Default | Limits | Meaning |
| --- | --- | --- | --- | --- |
| `target` | identifier | required | a declared target | The endpoint the load submits to |
| `source-ref` | reference | required | | The output that holds the flood transactions |
| `probes-ref` | reference | required | at least the probe budget | The output that holds the probe transactions |
| `state-check` | operation | with `state-ref` | `plugin.operation` | Checks chain state for a sample of included transactions, at a finalized block, after the load |
| `state-ref` | reference | with `state-check` | | The value the state check gets as `state` |
| `connections` | integer | `4` | 1 to 64 | WebSocket connections for submissions |
| `baseline-probes` | integer | `5` | 1 to 1000 | Baseline probes, one per block |
| `recovery-seconds` | integer | `900` | 0 to 86400 | Maximum recovery time |
| `block-interval-seconds` | number | `6.0` | at least 0.1 | The expected block time |

Preflight measures the block time. If it differs from `block-interval-seconds` by more than 25%, the run stops before setup.

The probe budget is the baseline probes, plus one probe per block of the recovery window, plus 10 spare. With the defaults, the budget is 5 + 900 / 6 + 10 = 165 probes. With fewer probes, the run fails with `insufficient probe reserve`.

### rate

A rate step sends flood transactions at one target rate for a set time. A plan holds at most 10000 rate steps.

| Attribute | Type | Default | Limits | Meaning |
| --- | --- | --- | --- | --- |
| `tx-per-second` | number | required | finite; at least 0 | Target rate, in transactions per second |
| `seconds` | integer | required | at least 1 | Rate step duration |

### ramp

A ramp makes a list of rate steps. The first rate step uses `start`. Each later rate step adds `step` to the rate before it, or multiplies it by `growth`.

| Attribute | Type | Default | Limits | Meaning |
| --- | --- | --- | --- | --- |
| `start` | number | required | finite; above 0 | The first target rate |
| `step` | number | one of `step` or `growth` | finite | Added to the rate at each rate step. No rate can fall below 0 |
| `growth` | number | one of `step` or `growth` | finite; above 0 | Multiplies the rate at each rate step |
| `steps` | integer | required | 1 to 10000 | Number of rate steps |
| `seconds` | integer | required | at least 1 | Duration of each rate step |

### monitors

Watches a Zombienet test network during the load. It records what the relay chain reports about the parachain, reads the listed metrics from each node and records [node CPU and memory](troubleshooting.md#node--no-cpu-or-memory-recorded). It needs a `<load>`.

Children: `metric`, zero or more.

| Attribute | Type | Default | Limits | Meaning |
| --- | --- | --- | --- | --- |
| `topology` | string | required | a name from `polkameter plugin topology` | The network's `zombie.json` on this host |
| `relay-target` | identifier | required | a declared target | The relay chain endpoint |
| `para-id` | integer | required | 0 to 4294967295 | The parachain to scrape and check |

### metric

A metric that every node of one role must expose.

| Attribute | Type | Default | Limits | Meaning |
| --- | --- | --- | --- | --- |
| `role` | string | required | `collator`, `collator-relay` or `validator` | The node role |
| `name` | string | required | a Prometheus metric family | The metric the role must expose |

If a node does not have a required metric, the run stops before setup. If no node has the role, preflight fails.

### thresholds

Sets the limits for the failure onset, the stop rules, recovery and finality. Each value replaces its default. [Stop rules](reading-results.md#stop-rules) explains each rule.

| Attribute | Type | Default | Limits | Governs |
| --- | --- | --- | --- | --- |
| `min-included-ratio` | number | `0.9` | above 0, at most 1 | Share of a step's transactions that must be included |
| `max-p95-latency-ms` | integer | `10000` | above 0 | p95 time from submission to a best block |
| `max-refused-ratio` | number | `0.01` | above 0, at most 1 | Share of a step's submissions that the node refuses |
| `pool-refuses-ratio` | number | `0.1` | above 0, at most 1 | Refused share that ends the ramp gracefully |
| `max-submit-reply-ms` | integer | `5000` | above 0 | p95 submission reply time |
| `min-send-ratio` | number | `0.95` | above 0, at most 1 | Share of the target rate PolkaMeter must send |
| `max-block-gap-factor` | number | `2.0` | above 0 | Mean block gap, as a multiple of `block-interval-seconds` |
| `stall-ms` | integer | `30000` | above 0 | Time without a new best block |
| `finality-stall-ms` | integer | `60000` | above 0 | Time without a new finalized block |
| `probes-in-a-row` | integer | `3` | above 0 | Recovery probes in a row that must be [in time](reading-results.md#did-the-chain-recover) |
| `recovered-block-gap-factor` | number | `1.5` | above 0 | Mean block gap that counts as recovered |
| `finality-wait-ms` | integer | `180000` | above 0 | Wait for finality after the last block with a run transaction |

### step

A plan step: one call to a plugin operation or a built-in operation. It isn't a [rate step](#rate). Children: `input`, zero or more.

| Attribute | Type | Default | Limits | Meaning |
| --- | --- | --- | --- | --- |
| `id` | identifier | required | unique in the plan | Its outputs are `steps.ID.OUTPUT` |
| `use` | operation | required | `plugin.operation` of a declared plugin, or a built-in operation | The operation to run |
| `timeout-ms` | integer | `60000` | above 0 | The step's deadline |

> **Warning:** If a submit step times out, the chain may already have the transaction. Its outcome is unknown. PolkaMeter does not retry it.

#### Built-in operations

| Operation | Inputs | Outputs | Behavior |
| --- | --- | --- | --- |
| `core.echo` | `value` | `value` | Returns its input. Read-only |
| `core.assert-equal` | `actual`, `expected` | `passed` | Fails the step, and so the run, if the values differ. Read-only |
| `core.submit-prepared` | `target`, `transactions` | `hashes`, `at` | Submits prepared transactions and waits for the RPC reply. `at` is empty |
| `core.submit-setup` | `target`, `transactions` | `hashes`, `at` | Also waits for finalized inclusion. Fails if an extrinsic or a Sudo call failed. `at` is the finalized block hash |

### input

Sets one input of a step.

| Attribute | Type | Default | Limits | Meaning |
| --- | --- | --- | --- | --- |
| `name` | string | required | unique in the step | The operation input |
| `ref` | reference | one of `ref` or `value` | | A value from [References](#references) |
| `value` | literal | one of `ref` or `value` | | Parsed as JSON when it can be, otherwise a string |

For quotes inside a JSON literal, use XML escapes, for example `value="[&quot;a&quot;]"`.

## References

| Reference | Value | Available |
| --- | --- | --- |
| `steps.ID.OUTPUT` | An output of an earlier step | Later steps |
| `targets.ID` | The endpoint string | After `<targets>` |
| `credentials.ID` | The secret, read on this host | Everywhere. [Redacted](results.md#redaction) |
| `run.id` | The run ID, such as `run-1791091539106-57209-1` | Everywhere |
| `run.directory` | The absolute path of the run directory | Once the directory exists |
| `run.probeBudget` | The probe budget, or `0` without a `<load>` | Everywhere |

`validate` checks that a referenced step exists. `preflight` checks the output name against the plugin's manifest.

## Validation rules

The [XSD schema](https://github.com/agustinustheo/polkameter/blob/main/schemas/polkameter-plan.xsd) lets an XML editor catch most mistakes. `polkameter validate` checks the rest.

The plan file must be UTF-8 and at most 2 MiB. Also:

| Item | Limit |
| --- | --- |
| XML | No DOCTYPE, CDATA, text inside elements or processing instructions. The XML declaration and comments are fine. At most 16 levels deep |
| Elements and attributes | No unknown names. Every required attribute is present |
| IDs | Unique per kind. Step IDs are unique in the whole plan |
| Transaction lists | At most 100000 transactions. Each transaction's bytes match its hash |

A plan error exits `2`, usually with a line and column.
