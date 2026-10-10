# Plans

A plan is one XML document in the namespace `https://polkameter.dev/schema/plan`. It names the plugins it uses, the targets and credential profiles, the steps to run, the load profile, the monitors and the stop thresholds. It never contains a secret, an executable path or a topology path. Those are configured on the host where the plan runs.

The [XML schema](https://github.com/agustinustheo/polkameter/blob/main/schemas/polkameter-plan.xsd) defines the structure. The host also resolves references, checks each plugin operation against its manifest, and checks limits and versions. This page lists both.

## Document shape

Children appear in this order. Each container appears at most once, except `workflow`.

```text
polkameter-plan
  plugins       plugin*
  targets       target*
  credentials   credential*
  preflight     step*
  setup         step*
  workflow      step*              (zero or more workflows)
  load          rate+ or ramp      (optional)
  monitors      metric*            (optional, needs load)
  thresholds                       (optional)
  evaluate      step*
  teardown      step*
```

## polkameter-plan

| Attribute | Type | Required | Default | Meaning |
| --- | --- | --- | --- | --- |
| `xmlns` | URI | yes | none | Must be `https://polkameter.dev/schema/plan` |
| `name` | string | yes | none | The scenario title, used in the report |
| `version` | integer | yes | none | Must be `1` |
| `mode` | `stress` or `smoke` | no | `stress` | Smoke mode also fails on monitor problems and on required checks without a result. See [results](results.md) |
| `timeout-ms` | integer above 0 | no | `3600000` | Deadline for the run from plugin start-up to the report. Teardown has its own 30 second limit |

## plugins and plugin

Every operation a step uses must belong to a plugin listed here, or to the built-in `core` operations.

| Attribute | Type | Required | Meaning |
| --- | --- | --- | --- |
| `id` | identifier | yes | Unique. `core` is reserved |
| `version` | string | yes | Must equal the installed plugin's version |
| `protocol` | integer | yes | Must be `1` |

## targets and target

| Attribute | Type | Required | Meaning |
| --- | --- | --- | --- |
| `id` | identifier | yes | Unique. Referenced as `targets.ID` |
| `endpoint` | URI | yes | A WebSocket RPC endpoint: `ws://` or `wss://` |

## credentials and credential

| Attribute | Type | Required | Meaning |
| --- | --- | --- | --- |
| `id` | identifier | yes | Unique. Referenced as `credentials.ID` |
| `profile` | string | yes | The credential profile this host maps to an environment variable with `polkameter plugin credential` |

The secret is resolved on the host when the run starts. It is redacted from events and from error messages.

## Steps: preflight, setup, workflow, evaluate and teardown

Each of these containers holds `step` elements. Preflight, setup, evaluate and teardown contain steps directly. A `workflow` contains steps and also has its own attributes.

### workflow

| Attribute | Type | Required | Default | Limits | Meaning |
| --- | --- | --- | --- | --- | --- |
| `id` | identifier | yes | none | unique | The workflow's name in events |
| `users` | integer | no | `1` | 1 to 100000 | Simulated users |
| `iterations` | integer | no | `1` | at least 1 | Iterations each user runs |
| `concurrency` | integer | no | `1` | 1 to 1000 | Users running at the same time |

### step

| Attribute | Type | Required | Default | Meaning |
| --- | --- | --- | --- | --- |
| `id` | identifier | yes | none | Unique across the whole plan. Its outputs are `steps.ID.OUTPUT` |
| `use` | `plugin.operation` or `core.name` | yes | none | The operation to run |
| `timeout-ms` | integer above 0 | no | `60000` | Deadline for the step |

A step that submits a transaction and times out may already have been accepted. Its outcome is then unknown, and Polkameter never retries it automatically.

### input

Each `input` has a `name` (unique within its step) and exactly one of `ref` or `value`.

| Attribute | Type | Meaning |
| --- | --- | --- |
| `name` | string | The operation input this sets |
| `ref` | reference | A value from the reference table below |
| `value` | literal | A literal. It parses as JSON when it can, and is otherwise a string |

Literals are not evaluated. Use XML escaping for quotes in JSON strings, for example `value="[&quot;a&quot;]"`.

### Built-in operations

| Operation | Inputs | Outputs | Behavior |
| --- | --- | --- | --- |
| `core.echo` | `value` | `value` | Returns its input. Read-only |
| `core.assert-equal` | `actual`, `expected` | `passed` | Fails the step, and so the run, unless the values are equal. Read-only |
| `core.submit-prepared` | `target`, `transactions` | `hashes`, `at` | Submits prepared transactions and waits for the RPC reply. `at` is empty |
| `core.submit-setup` | `target`, `transactions` | `hashes`, `at` | As above, and waits for finalized inclusion and checks the dispatch and Sudo results. `at` is the finalized block hash |

Preflight steps may use only read-only operations.

## load

| Attribute | Type | Required | Default | Limits | Meaning |
| --- | --- | --- | --- | --- | --- |
| `target` | identifier | yes | none | a declared `targets` ID | The endpoint the load submits to |
| `source-ref` | reference | yes | none | | The output that holds the flood transactions |
| `probes-ref` | reference | yes | none | | The output that holds the reserved probe transactions. Disjoint from the flood, and each hash is sent at most once |
| `state-check` | operation | with `state-ref` | none | | A plugin operation run on a sample of finalized transactions after recovery |
| `state-ref` | reference | with `state-check` | none | | The input the state check receives as `state` |
| `connections` | integer | no | `4` | 1 to 64 | WebSocket connections used for submissions |
| `baseline-probes` | integer | no | `5` | 1 to 1000 | Probe transactions sent before the load, one per block |
| `recovery-seconds` | integer | no | `900` | 0 to 86400 | How long recovery may last before the run gives up |
| `block-interval-seconds` | number | no | `6.0` | at least 0.1 | The block time the plan expects. Preflight compares it with the measured value |

The `state-check` operation receives `target`, `state`, `hashes` and `at`, and returns `checked`, `missing` and `detail`.

A load has one of two profiles.

- **Rate steps.** One or more `<rate tx-per-second="4" seconds="30"/>` elements. Each runs at its rate for its duration. `tx-per-second` is a finite number of at least 0, and `seconds` is at least 1.
- **Ramp.** One `<ramp start="2" step="2" steps="3" seconds="30"/>`. Give exactly one of `step`, which adds that much per step, or `growth`, which multiplies the rate by that factor per step. `start` must be above 0, and `steps` is at most 10000.

Rate steps and a ramp cannot be combined. A load with neither is rejected. A plan has at most 10000 rate steps.

The probe budget is `run.probeBudget`: `baseline-probes`, plus `recovery-seconds` divided by `block-interval-seconds` and rounded up, plus 10 spare.

Preflight measures the timestamp span of recent blocks and compares it with `block-interval-seconds`. If the two differ by more than 25%, the run stops before setup, and the measurement is written to `calibration.json`.

## monitors

| Attribute | Type | Required | Meaning |
| --- | --- | --- | --- |
| `topology` | identifier | yes | The topology alias this host registered with `polkameter plugin topology` |
| `relay-target` | identifier | yes | A declared `targets` ID for the relay chain, read by the relay recorder |
| `para-id` | integer (not range-checked) | yes | The parachain whose collators are scraped and whose relay slots are judged |

Monitors need a `<load>`. Each `metric` child has a `role` and a `name`:

| Attribute | Type | Required | Meaning |
| --- | --- | --- | --- |
| `role` | string: `collator`, `collator-relay` or `validator` (other values fail at preflight) | yes | The node role the metric must exist on |
| `name` | metric family | yes | A Prometheus metric family that every node of that role must expose |

If a required metric is missing, the run stops before setup. Wrong-type metrics stop the run only for families in Polkameter's built-in metric registry. A `role` that no node has fails at preflight with `required role X has no nodes`.

## thresholds

Thresholds override the defaults in `crates/load/src/rules.rs`. Each value is written to `summary.json` under `rules`. The meaning of each measure is in [results](results.md#stop-rules-and-thresholds).

| Attribute | Type | Default | Constraint | Governs |
| --- | --- | --- | --- | --- |
| `min-included-ratio` | ratio | `0.9` | above 0, at most 1 | Share of a step's transactions that must be included |
| `max-p95-latency-ms` | integer | `10000` | above 0 | p95 time from send to a best block |
| `max-refused-ratio` | ratio | `0.01` | above 0, at most 1 | Share of a step's submissions refused |
| `pool-refuses-ratio` | ratio | `0.1` | above 0, at most 1 | Refused share that ends the ramp as a graceful failure |
| `max-submit-reply-ms` | integer | `5000` | above 0 | p95 submission reply time, the pool intake rule |
| `min-send-ratio` | ratio | `0.95` | above 0, at most 1 | Share of the target rate the load tool must send |
| `max-block-gap-factor` | number | `2.0` | above 0 | Mean block gap, as a multiple of the interval at the start |
| `stall-ms` | integer | `30000` | above 0 | Time without a new best block |
| `finality-stall-ms` | integer | `60000` | above 0 | Time without a new finalized block |
| `probes-in-a-row` | integer | `3` | above 0 | Recovery probes in a row that must land in time |
| `recovered-block-gap-factor` | number | `1.5` | above 0 | Mean block gap that counts as recovered, as a multiple of the interval |
| `finality-wait-ms` | integer | `180000` | above 0 | How long reconciliation waits for the chain to finalize past the last block that holds one of the run's transactions |

## evaluate

Evaluate steps run after measurement and before the report. An operation whose outputs include `checks` contributes its checks to the report. [Plugins](plugins.md) describes the check format.

## Reference syntax

A `ref` names one of the values below. Anything else is rejected when the plan is parsed.

| Reference | Value | Available |
| --- | --- | --- |
| `steps.ID.OUTPUT` | An output of an earlier step, named as its operation declares it | Later steps. The output name is checked before setup |
| `targets.ID` | The endpoint string | After `<targets>` |
| `credentials.ID` | The secret, resolved on this host | Wherever the credential is declared. Redacted from events |
| `run.id` | The run ID, such as `run-1791091539106-57209-1` | Everywhere |
| `run.directory` | The absolute path of this run's directory | Everywhere, once the directory exists |
| `run.probeBudget` | The probe transactions the plan needs; `0` without a `<load>` | Everywhere |
| `user.index` | The user's index, from 0 | Inside `workflow` steps |
| `iteration.index` | The iteration's index, from 0 | Inside `workflow` steps |

Scoping rules:

- A reference must name something declared earlier in the plan. Forward references are rejected.
- Setup outputs are copied into every user and iteration of each workflow.
- A workflow step's outputs are visible only to later steps in the same iteration. They do not reach another user, iteration or workflow.
- Evaluate and teardown steps can reference preflight and setup outputs. They cannot reference workflow outputs.

## Validation rules

Rules fall into four groups: parsing, structure, plugin contracts and chain checks.

**Parsing** happens on every command that reads a plan, including `validate`.

- The file is UTF-8 text no larger than 2 MiB. A leading UTF-8 byte-order mark is accepted.
- It contains no DOCTYPE.
- Processing instructions are rejected, except a leading `<?xml ...?>` declaration, which must have `version`.
- CDATA and other `<!` declarations are rejected. Comments are allowed.
- Text inside elements is rejected. Only elements, whitespace and comments may appear there.
- Nesting deeper than 16 levels is rejected.
- In attribute values, `<` and duplicate attributes are rejected. Only the five predefined entities and numeric character references to valid XML characters are decoded. Literal tabs and newlines become spaces.
- Numeric attributes may have spaces around the value.
- The root element is `polkameter-plan`.
- Unknown elements and attributes are rejected, and required attributes must be present.
- `version` is `1`, `xmlns` is the schema namespace, and `mode` is `stress` or `smoke`.
- Thresholds are within the ranges above.

**Structure** is checked by the same validation step.

- Identifiers use ASCII letters, digits, `-` and `_`.
- IDs are unique for plugins, targets, credentials, workflows and steps. Step IDs are unique across the whole plan.
- A target endpoint starts with `ws://` or `wss://`.
- An operation is `core.*` or `plugin.operation`, where `plugin` is declared in `<plugins>`.
- Each input has a unique name and exactly one of `ref` or `value`.
- Every reference names a known value declared earlier.
- Workflow, load and monitor limits hold, and a plan has at most 10000 rate steps.

**Plugin contracts** are checked once the installed plugins are started, which is the first thing a run does after parsing. `polkameter plugin inspect` performs the same check without connecting to a chain.

- Each installed plugin's version equals the plan entry's version, and its executable still matches the BLAKE2 hash pinned at `plugin install`.
- Each operation's inputs and outputs match its manifest schema.
- Preflight steps use only read-only operations.

**Chain checks** happen in the preflight phase of a run. They need a reachable chain.

- Required evidence from manifests is present: a provisioner capability, an RPC method, or a metric.
- The block interval is calibrated when there is a load.

When a transaction source is read, it may hold at most 100000 transactions, and each prepared transaction's bytes must match its hash.

`polkameter validate` runs the parsing and structure checks only. `polkameter preflight` runs the read-only preflight steps and the chain checks, and needs the chain.

## Errors

Parse and validation errors are returned as a message that names the problem. The CLI prints it after `Polkameter:` and exits with code 2. XML syntax errors, and errors about an element or attribute, name the line and column, for example `line 3, column 6: expected </c>, found </d>`. Errors from the validation step, such as references, limits and IDs, name the problem without a position.

## Example

The example plan from the repository runs without a chain. It uses the example plugin and the built-in `core.echo` and `core.assert-equal` operations.

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

Read it in order:

- `<plugins>` declares the `example` plugin at version `0.1.0`. The host checks this against the installed plugin.
- `seed` runs `example.double` on the literal `21`, giving `42`. Its output is `steps.seed.value`.
- `copied` echoes that value, so `steps.copied.value` is `42` and is copied into every user and iteration.
- The workflow runs 3 users with 2 iterations each, 2 at a time. Each iteration doubles `42` to `84`.
- `verified` asserts that the doubled value equals the literal `84`. If the assertion fails, the step fails and the run exits with code 1.

The [smoke test](https://github.com/agustinustheo/polkameter/blob/main/tests/cli-smoke.sh) changes `expected` to `85` to check that a failed assertion exits nonzero. The plan has no `<load>`, so it produces no transaction files.
