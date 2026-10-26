# Plan reference

A plan is one XML file that describes a test: the plugins it uses, the chain endpoints, the credential profiles, the steps to run, an optional load test, the monitors and the stop thresholds. A plan never contains a secret, an executable path or a topology path. You set those on the host where the plan runs, with [`polkameter plugin`](cli.md#plugin).

## Example

This plan runs without a chain. It uses the example plugin and the built-in `core` operations, and it is the plan in `examples/plugin-workflow.polkameter.xml`:

```xml
<?xml version="1.0" encoding="UTF-8"?>
<polkameter-plan xmlns="https://polkameter.dev/schema/plan" version="1" name="Plugin data flow" timeout-ms="30000">
  <plugins><plugin id="example" version="0.1.0" protocol="1"/></plugins>
  <setup>
    <step id="seed" use="example.double"><input name="value" value="21"/></step>
    <step id="copied" use="core.echo"><input name="value" ref="steps.seed.value"/></step>
  </setup>
  <workflow id="users" users="3" iterations="2" concurrency="2">
    <step id="computed" use="example.double"><input name="value" ref="steps.copied.value"/></step>
    <step id="verified" use="core.assert-equal">
      <input name="actual" ref="steps.computed.value"/><input name="expected" value="84"/>
    </step>
  </workflow>
</polkameter-plan>
```

Run from the repository root:

```sh
polkameter validate examples/plugin-workflow.polkameter.xml
```

```text
Plan is structurally valid. Preflight checks the installed plugins.
```

- `seed` doubles `21` to `42`.
- `copied` passes that output to every user and iteration, so `steps.copied.value` is `42` in each one.
- Each iteration doubles `42` to `84`. `verified` fails the run, with exit code 1, if the result is not `84`.

The next example adds targets, credentials, a load test, monitors and thresholds. Its plugin, operations and endpoints are placeholders. It passes `validate`, but it cannot run until you replace them:

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

- `targets.para` is the endpoint the load submits to. `credentials.admin` names the `sudo` profile, not a secret.
- `prepare` runs in setup, so the load can read its outputs `flood`, `probes` and `expected`.
- `<load>` sends the flood at 20, then 40 transactions per second, for 60 seconds each.
- `<monitors>` needs the `<load>`. It requires every collator to expose `substrate_block_height`.
- `<thresholds>` changes two stop rules. Every other threshold keeps its default.

## Table columns

- **Required** is `yes`, `no`, or the rule that applies when the attribute depends on another one. **Default** is the value used when the attribute is absent. "none" means there is no default, so you must set it when it is required.
- **Children** says how many child elements an element can hold: "zero or more", "one or more", or "optional" (at most one).
- An `ID` is a name you choose. IDs use ASCII letters, digits, `-` and `_`.
- The **Type** column uses these words: *integer* is a whole number, *number* is a decimal, *identifier* is an ID, *URI* is a web address, *reference* is a value from the [References](#references) table, and *operation* is `plugin.operation` or `core.NAME`.

## Elements

### polkameter-plan

The root element. Its `name` appears in the report.

Children, in any order:

| Element | Occurs | Section |
| --- | --- | --- |
| `plugins` | optional | [plugins](#plugins) |
| `targets` | optional | [targets](#targets) |
| `credentials` | optional | [credentials](#credentials) |
| `preflight` | optional | [preflight](#preflight) |
| `setup` | optional | [setup](#setup) |
| `workflow` | zero or more | [workflow](#workflow) |
| `load` | optional | [load](#load) |
| `monitors` | optional. Needs `load` | [monitors](#monitors) |
| `thresholds` | optional | [thresholds](#thresholds) |
| `evaluate` | optional | [evaluate](#evaluate) |
| `teardown` | optional | [teardown](#teardown) |

Each element appears at most once, except `workflow`. Execution follows the phases in the table, regardless of where you place their sections in the XML. Steps within a phase run in their written order.

| Attribute | Type | Required | Default | Limits | Meaning |
| --- | --- | --- | --- | --- | --- |
| `xmlns` | URI | yes | none | `https://polkameter.dev/schema/plan` | The plan namespace |
| `name` | string | yes | none | not empty | The scenario's title, used in the report |
| `version` | integer | yes | none | `1` | The plan format version |
| `mode` | string | no | `stress` | `stress` or `smoke` | Smoke mode also fails the run on monitor problems and on required checks without a result. See [results](results.md#exit-codes) |
| `timeout-ms` | integer | no | `3600000` | above 0 | The deadline for the run, from plugin start-up to the report. A run that reaches it ends as `failed`, with exit code 1 |

### plugins

Lists the plugins the plan uses. Every operation a step calls must belong to a plugin listed here, or be a `core` operation.

Children: `plugin`, zero or more.

### plugin

Declares one plugin. The version and protocol must match the plugin installed on the host.

| Attribute | Type | Required | Default | Limits | Meaning |
| --- | --- | --- | --- | --- | --- |
| `id` | identifier | yes | none | unique; not `core` | The plugin's name. Operations use it as their prefix, such as `example.double` |
| `version` | string | yes | none | not empty | Must equal the installed plugin's version. `preflight` and the start of a run check it |
| `protocol` | integer | yes | none | `1` | The plugin protocol the plan expects |

```xml
<plugin id="example" version="0.1.0" protocol="1"/>
```

### targets

Lists the chain endpoints the plan uses. A load test and monitors refer to them by ID.

Children: `target`, zero or more.

### target

| Attribute | Type | Required | Default | Limits | Meaning |
| --- | --- | --- | --- | --- | --- |
| `id` | identifier | yes | none | unique | Referenced as `targets.ID` |
| `endpoint` | URI | yes | none | starts with `ws://` or `wss://` | The chain's WebSocket RPC endpoint |

```xml
<target id="para" endpoint="ws://127.0.0.1:9988"/>
```

### credentials

Lists the credential profiles the plan uses. The plan names a profile, never the secret.

Children: `credential`, zero or more.

### credential

| Attribute | Type | Required | Default | Limits | Meaning |
| --- | --- | --- | --- | --- | --- |
| `id` | identifier | yes | none | unique | Referenced as `credentials.ID` |
| `profile` | string | yes | none | not empty | The profile this host maps to an environment variable with [`polkameter plugin credential`](cli.md#plugin) |

PolkaMeter reads the secret on the host when the run starts. It redacts the secret from events and error messages.

```xml
<credential id="admin" profile="sudo"/>
```

### preflight

Steps that check the chain before the run. Preflight steps may use only read-only operations.

Children: `step`, zero or more.

### setup

Steps that run once, before the workflows. Their outputs are copied into every user and iteration of each workflow.

Children: `step`, zero or more.

### workflow

A group of steps that simulated users run. Each user runs the steps once per iteration.

Children: `step`, zero or more. A plan can hold any number of workflows.

| Attribute | Type | Required | Default | Limits | Meaning |
| --- | --- | --- | --- | --- | --- |
| `id` | identifier | yes | none | unique | The workflow's name in events |
| `users` | integer | no | `1` | 1 to 100000 | Simulated users |
| `iterations` | integer | no | `1` | at least 1 | Iterations each user runs |
| `concurrency` | integer | no | `1` | 1 to 1000 | Users running at the same time |

```xml
<workflow id="users" users="3" iterations="2" concurrency="2">
```

### load

Turns the plan into a load test against one target. The load sends transactions at the rates you give, then measures how the chain responds. [How a run works](how-it-works.md) describes the phases.

Children: `rate` (one or more) or `ramp` (exactly one). Give one or the other, not both. A load with neither is rejected.

| Attribute | Type | Required | Default | Limits | Meaning |
| --- | --- | --- | --- | --- | --- |
| `target` | identifier | yes | none | a declared `targets` ID | The endpoint the load submits to |
| `source-ref` | reference | yes | none | a reference | The output that holds the flood transactions |
| `probes-ref` | reference | yes | none | a reference; must hold the probe budget (see below) | The output that holds the reserved probe transactions. No transaction may appear in both the flood and the probes, and each is sent at most once |
| `state-check` | operation | with `state-ref` | none | a `plugin.operation` | A plugin operation that checks chain state for a sample of included transactions, at a finalized block, after the load |
| `state-ref` | reference | with `state-check` | none | a reference | The input the state check receives as `state` |
| `connections` | integer | no | `4` | 1 to 64 | WebSocket connections used for submissions |
| `baseline-probes` | integer | no | `5` | 1 to 1000 | Probe transactions sent before the load, one per block |
| `recovery-seconds` | integer | no | `900` | 0 to 86400 | How long recovery may last before the run gives up |
| `block-interval-seconds` | number | no | `6.0` | at least 0.1 | The block time the plan expects. Preflight compares it with the measured block time |

`state-check` and `state-ref` must appear together. The CLI checks this pairing; XSD 1.0 cannot express dependencies between attributes. The state check receives `target`, `state`, `hashes` and `at`, and returns `checked`, `missing` and `detail`.

If the measured block time differs from `block-interval-seconds` by more than 25%, the run stops before setup. The measurement is written to `calibration.json`.

**Probe budget.** The probe output must hold at least the probe budget. The budget is the baseline probes, plus one probe per block over the recovery window, plus 10 spare. The recovery part rounds up to a whole block. With the defaults, the budget is 5 + 900 / 6 + 10 = 165 probes: 5 baseline probes, 150 for recovery, and 10 spare. If the output holds fewer, the run fails with `insufficient probe reserve`.

```xml
<load target="para" source-ref="steps.prepare.flood" probes-ref="steps.prepare.probes">
```

### rate

One rate step: a fixed rate for a fixed time. A load has one or more rate steps, and a plan holds at most 10000.

Children: none.

| Attribute | Type | Required | Default | Limits | Meaning |
| --- | --- | --- | --- | --- | --- |
| `tx-per-second` | number | yes | none | finite; at least 0 | Transactions per second |
| `seconds` | integer | yes | none | at least 1 | How long the step runs |

```xml
<rate tx-per-second="4" seconds="30"/>
```

### ramp

A load that changes at each step. It starts at `start` and changes by `step` or `growth` for each of its `steps`.

Children: none.

| Attribute | Type | Required | Default | Limits | Meaning |
| --- | --- | --- | --- | --- | --- |
| `start` | number | yes | none | finite; above 0 | The rate of the first step, in transactions per second |
| `step` | number | one of `step` or `growth` | none | finite | Adds this many transactions per second at each step. No rate may fall below 0 |
| `growth` | number | one of `step` or `growth` | none | finite; above 0 | Multiplies the rate by this factor at each step |
| `steps` | integer | yes | none | 1 to 10000 | The number of steps |
| `seconds` | integer | yes | none | at least 1 | How long each step runs |

Give exactly one of `step` or `growth`. XSD 1.0 choices apply to child elements, so the CLI checks these exclusive attributes. The CLI also expands the ramp and rejects any rate that is negative or not finite.

```xml
<ramp start="2" step="2" steps="3" seconds="30"/>
```

### monitors

Watches the chain during the load test. It records the chain, scrapes the metrics you list, and records the node's CPU and memory.

Children: `metric`, zero or more. Needs a `<load>`.

| Attribute | Type | Required | Default | Limits | Meaning |
| --- | --- | --- | --- | --- | --- |
| `topology` | string | yes | none | an alias registered with `polkameter plugin topology` | The topology alias on this host |
| `relay-target` | identifier | yes | none | a declared `targets` ID | The relay chain endpoint that the relay recorder reads |
| `para-id` | integer | yes | none | 0 to 4294967295 | The parachain whose collators are scraped and whose relay slots are judged |

Zero is accepted because the SDK's [`ParaId` is an unsigned 32-bit integer](https://docs.rs/polkadot-parachain-primitives/22.0.0/src/polkadot_parachain_primitives/primitives.rs.html). Its system range includes 0 through 1999; the public range starts at 2000. Plan validation does not check whether the ID is registered on your relay chain.

```xml
<monitors topology="test-network" relay-target="relay" para-id="2000">
```

### metric

A metric that every node of one role must expose.

Children: none.

| Attribute | Type | Required | Default | Limits | Meaning |
| --- | --- | --- | --- | --- | --- |
| `role` | string | yes | none | `collator`, `collator-relay` or `validator` | The node role the metric must exist on. Any other value fails at preflight |
| `name` | string | yes | none | a Prometheus metric family | The metric family that every node of the role must expose |

If a node lacks a required metric, the run stops before setup. If no node has the role, preflight fails with `required role X has no nodes`. Separately, in a plan with `<monitors>`, a metric that PolkaMeter knows about stops the run if a node exposes it with a different type, whether or not the plan requires it.

```xml
<metric role="collator" name="substrate_block_height"/>
```

### thresholds

Changes the stop rules of the load test. Each value you set replaces its default. The effective values are written to `summary.json` under `rules`. [Results](results.md#stop-rules-and-thresholds) explains each measure.

Children: none.

| Attribute | Type | Default | Limits | Governs |
| --- | --- | --- | --- | --- |
| `min-included-ratio` | number | `0.9` | above 0, at most 1 | Share of a step's transactions that must be included |
| `max-p95-latency-ms` | integer | `10000` | above 0 | p95 time from send to a best block |
| `max-refused-ratio` | number | `0.01` | above 0, at most 1 | Share of a step's submissions that are refused |
| `pool-refuses-ratio` | number | `0.1` | above 0, at most 1 | Refused share that ends the ramp as a graceful failure |
| `max-submit-reply-ms` | integer | `5000` | above 0 | p95 submission reply time, the pool intake rule |
| `min-send-ratio` | number | `0.95` | above 0, at most 1 | Share of the target rate the load tool must send |
| `max-block-gap-factor` | number | `2.0` | above 0 | Mean block gap, as a multiple of the interval at the start |
| `stall-ms` | integer | `30000` | above 0 | Time without a new best block |
| `finality-stall-ms` | integer | `60000` | above 0 | Time without a new finalized block |
| `probes-in-a-row` | integer | `3` | above 0 | Recovery probes in a row that must land in time |
| `recovered-block-gap-factor` | number | `1.5` | above 0 | Mean block gap that counts as recovered, as a multiple of the interval |
| `finality-wait-ms` | integer | `180000` | above 0 | How long reconciliation waits for the chain to finalize past the last block that holds one of the run's transactions |

### evaluate

Steps that run after the load and before the report. An operation whose outputs include `checks` adds its checks to the report. [Plugins](plugins.md) describes the check format.

Children: `step`, zero or more.

### teardown

Steps that clean up at the end of the run. They have their own 30-second limit.

Children: `step`, zero or more.

### step

One call to a plugin operation or a built-in operation. Steps appear in `preflight`, `setup`, `workflow`, `evaluate` and `teardown`.

Children: `input`, zero or more.

| Attribute | Type | Required | Default | Limits | Meaning |
| --- | --- | --- | --- | --- | --- |
| `id` | identifier | yes | none | unique across the whole plan | Its outputs are `steps.ID.OUTPUT` |
| `use` | operation | yes | none | `plugin.operation` for a declared plugin, or a built-in name from the table below | The operation to run |
| `timeout-ms` | integer | no | `60000` | above 0 | The step's deadline |

> **Warning:** If a submit step times out, the chain may already have accepted the transaction, and its outcome is unknown. PolkaMeter does not retry it.

```xml
<step id="computed" use="example.double"><input name="value" ref="steps.copied.value"/></step>
```

#### Built-in operations

| Operation | Inputs | Outputs | Behavior |
| --- | --- | --- | --- |
| `core.echo` | `value` | `value` | Returns its input. Read-only |
| `core.assert-equal` | `actual`, `expected` | `passed` | Fails the step, and so the run, unless the values are equal. Read-only |
| `core.submit-prepared` | `target`, `transactions` | `hashes`, `at` | Submits prepared transactions and waits for the RPC reply. `at` is empty |
| `core.submit-setup` | `target`, `transactions` | `hashes`, `at` | As above. It also waits for finalized inclusion, fails if an extrinsic failed, and checks the Sudo result of sudo transactions. `at` is the finalized block hash |

Any other `core.` name fails at preflight.

### input

Sets one input of a step, from an earlier output or from a literal.

Children: none.

| Attribute | Type | Required | Default | Limits | Meaning |
| --- | --- | --- | --- | --- | --- |
| `name` | string | yes | none | unique within the step | The operation input this sets |
| `ref` | reference | one of `ref` or `value` | none | a reference | A value from the [References](#references) table |
| `value` | literal | one of `ref` or `value` | none | | A literal. It parses as JSON when it can, and is otherwise a string |

Give exactly one of `ref` or `value`. The CLI checks these exclusive attributes because XSD 1.0 cannot express this rule.

Literals are not evaluated. For quotes inside a JSON string, use XML escaping, for example `value="[&quot;a&quot;]"`.

```xml
<input name="expected" value="84"/>
```

## References

A `ref` names one of the values below. A reference to anything else is rejected when the plan is validated.

| Reference | Value | Available |
| --- | --- | --- |
| `steps.ID.OUTPUT` | An output of an earlier step, named as its operation declares it | Later steps |
| `targets.ID` | The endpoint string | After `<targets>` |
| `credentials.ID` | The secret, read on this host | Wherever the credential is declared. Redacted from events |
| `run.id` | The run ID, such as `run-1791091539106-57209-1` | Everywhere |
| `run.directory` | The absolute path of this run's directory | Everywhere, once the directory exists |
| `run.probeBudget` | The probe budget of the plan; `0` without a `<load>` | Everywhere |
| `user.index` | The user's index, from 0 | Inside `workflow` steps |
| `iteration.index` | The iteration's index, from 0 | Inside `workflow` steps |

- A reference must name a value available in its execution phase. Phase order follows the table above, regardless of section order in the XML. Within a phase, forward step references are rejected.
- Setup outputs are copied into every user and iteration of each workflow.
- A workflow step's outputs are visible only to later steps in the same iteration. They do not reach another user, iteration or workflow.
- Evaluate and teardown steps can reference preflight and setup outputs. They cannot reference workflow outputs.
- `polkameter validate` checks that the step exists. It does not check the output name. `preflight` checks the name against the operation's manifest and fails with `unknown output steps.ID.OUTPUT`.

## Validation rules

The parser and validator run on every command that reads a plan. `polkameter validate` runs the parsing and structure checks only. `polkameter preflight` also checks the installed plugins and runs the read-only preflight steps. A plan with a load, or with steps that read the chain, needs a reachable chain for preflight to pass.

| Element | Limit |
| --- | --- |
| Plan file | At most 2 MiB. UTF-8. A leading byte-order mark is accepted |
| `polkameter-plan` | `version` is `1`. `xmlns` is the schema namespace. `name` is not empty. `timeout-ms` is above 0 |
| `plugin` | `protocol` is `1`. `id` is not `core` |
| `target` | `endpoint` starts with `ws://` or `wss://` |
| `credential` | `profile` is not empty |
| `workflow` | `users` 1 to 100000. `concurrency` 1 to 1000. `iterations` at least 1 |
| `step` | `timeout-ms` is above 0 |
| `load` | `connections` 1 to 64. `baseline-probes` 1 to 1000. `recovery-seconds` 0 to 86400. `block-interval-seconds` at least 0.1 |
| `rate` | `tx-per-second` finite and at least 0. `seconds` at least 1 |
| `ramp` | `start` above 0. `growth` above 0. `steps` 1 to 10000. `seconds` at least 1. No rate below 0 |
| Whole plan | At most 10000 rate steps |
| `monitors` | Needs a `<load>` and a declared `relay-target` |
| `thresholds` | Ratios above 0 and at most 1. Other values above 0 |
| Transaction source | At most 100000 transactions. Each prepared transaction's bytes must match its hash |

**Parsing** rejects:

- a DOCTYPE, CDATA, or any other `<!` declaration except a comment;
- processing instructions, except a leading `<?xml ...?>` declaration;
- text inside elements;
- nesting deeper than 16 levels;
- `<` in an attribute value, and duplicate attributes;
- unknown elements and attributes, and missing required attributes.

Numeric attributes may have spaces around the value. Integer limits also follow their storage type: durations in milliseconds use 1 to 18446744073709551615; durations in seconds and workflow iterations use 1 to 4294967295 unless a smaller limit is listed. Native-size counters such as `probes-in-a-row` use the host's unsigned integer limit (18446744073709551615 on a 64-bit host).

Numbers must be finite. Attribute strings are taken exactly as written; an empty `input` name, metric name, metric role or topology alias passes `validate`, while preflight checks the operation inputs, metrics and topology.

**Structure** checks:

- IDs are unique for plugins, targets, credentials, workflows and steps. Step IDs are unique across the whole plan.
- Each input has a unique name and exactly one of `ref` or `value`.
- Every reference names a value declared earlier.
- An operation is `core.NAME`, or `plugin.operation` where `plugin` is declared in `<plugins>`.

**Plugin contracts** are checked when the plugins start, which is the first thing a run does. `polkameter plugin inspect` runs the same check without a chain.

- Each installed plugin's version equals the plan's version, and its executable still matches the BLAKE2 hash recorded at `plugin install`.
- Each operation's inputs and outputs match its manifest.
- Preflight steps use only read-only operations.

**Chain checks** happen during preflight. They need a reachable chain. They check that the manifests' required evidence is present (a provisioner capability, an RPC method or a metric), and that the block interval matches the chain when there is a load.

**Schema coverage.** The [XSD 1.0 schema](https://github.com/agustinustheo/polkameter/blob/main/schemas/polkameter-plan.xsd) checks required attributes, value limits, identifiers, operation syntax, unique IDs and target references. It accepts sections in any order and applies the same attribute defaults as the CLI. CI validates every example plan and every complete documentation plan against both the schema and `polkameter validate` with `tests/validate-plans.py`.

Always run `polkameter validate` as well. The schema annotations identify rules that need CLI validation:

| Rule | CLI check beyond XSD 1.0 |
| --- | --- |
| Exclusive attributes | Exactly one of input `ref`/`value` and ramp `step`/`growth` |
| Paired attributes | Load `state-check` and `state-ref` appear together |
| Section counts | Every root child except `workflow` appears at most once. The schema uses a repeated choice to allow any section order and repeated workflows; XSD 1.0 `xs:all` cannot repeat workflows |
| References and providers | Output references exist in the current phase and scope; operation providers are declared. Monitors need a load |
| Expanded ramps | Every generated rate is finite and at least 0 |
| Host limits | Native-size counters also fit the host's unsigned integer type; the schema uses the 64-bit limit |
| Parsing | File size, depth, declarations and processing instructions follow the parser limits above. The CLI requires the default namespace on the root and unprefixed element names; XSD validates namespace identity |

### Errors

The CLI prints each error as `Polkameter: <message>` on standard error and exits with code 2. XML syntax errors and errors about an element or attribute give a line and column, such as `line 3, column 6: expected </c>, found </d>`. Other errors, such as references, limits and IDs, name the problem without a position. See [exit codes](cli.md#exit-codes).

For what each phase does, see [How a run works](how-it-works.md).
