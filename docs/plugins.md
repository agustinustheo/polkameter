# Plugins and SDK

A plugin is an executable that PolkaMeter starts for a run. Plans call its operations by name, and it replies over standard input and output. [Writing a plugin](writing-a-plugin.md) builds one.

## Example

A setup step calls `hello.greet`:

```xml
<plugins><plugin id="hello" version="0.1.0" protocol="1"/></plugins>
<setup>
  <step id="greet" use="hello.greet"><input name="name" value="Polkadot"/></step>
</setup>
```

## Manifest

`$describe` returns the manifest. The host checks it at install and at run start.

| Field | Type | Default | Meaning |
| --- | --- | --- | --- |
| `id` | string | required | The plugin's name in plans. Not empty or `core`. |
| `version` | string | required | Must equal the plan's `<plugin>` `version`. |
| `protocol` | integer | required | `1`. Must match the plan's `protocol`. |
| `operations` | map of name to operation | required | The operations a plan can call. |
| `requirements` | list | `[]` | Evidence checked before setup. |

Each operation has these fields:

| Field | Type | Default | Meaning |
| --- | --- | --- | --- |
| `description` | string | required | Shown by `plugin inspect`. |
| `inputs` | map of field | required | Inputs the operation accepts. |
| `outputs` | map of field | required | Outputs the operation must return. |
| `read_only` | boolean | `false` | `true` if it changes nothing on a chain and submits nothing. Only these run in `<preflight>`. |

Each input and output has a `schema` (required; see [Schemas](#schemas)) and `optional` (boolean, default `false`; `true` lets the field be absent).

A requirement names evidence the host checks before setup:

| Field | Type | Default | Meaning |
| --- | --- | --- | --- |
| `kind` | string | required | `rpc`, `metric` or `capability`. |
| `target` | string | required | `rpc`: target ID in the plan. `metric`: monitor role. Unused by `capability`. |
| `name` | string | required | `rpc`: RPC method. `metric`: metric family. `capability`: capability name. |
| `required` | boolean | required | Optional requirements are not checked. |

`rpc` passes when the target node lists the method in `rpc_methods`, `metric` when every node of the role exposes the family, and `capability` when [`polkameter plugin capability`](cli.md#plugin) recorded the name. Other kinds fail when `required` is true. The CI script `scripts/verify-previewnet-requirements.py` allows only `metric` and `rpc` requirements, and the metric roles `collator`, `collator-relay` and `validator`.

### Schemas

Each `schema` has a `type` key:

| Type | Rust | Accepts |
| --- | --- | --- |
| `string` | `Schema::String` | JSON string. |
| `number` | `Schema::Number` | JSON number. |
| `integer` | `Schema::Integer` | JSON integer. |
| `boolean` | `Schema::Boolean` | JSON boolean. |
| `array` | `Schema::Array { items }` | Array whose items match `items`. |
| `object` | `Schema::Object { fields }` | Declared fields. An unknown field fails, and a missing one fails unless `optional`. |
| `json` | `Schema::Json` | Any value. |

Inputs are checked before the call, outputs when the plugin replies. A mismatch becomes an error:

```json
{"protocol":1,"id":3,"result":null,"error":"value does not match String"}
```

A plan passes each input with `value` or `ref="steps.ID.OUTPUT"`. [Plans](plans.md#input) has the rules.

### Values between steps

- `steps.ID.OUTPUT` must name an output the step's operation declares. A forward or unknown reference fails before setup.
- Setup outputs are copied into every user and iteration. Workflow outputs cannot reach another user, iteration or workflow.
- The host sets `run.id`, `run.directory` and the [probe budget](plans.md#load). `user.index` and `iteration.index` identify the current user and iteration.

## Protocol

PolkaMeter starts one process per plugin per run and keeps it until the run ends. Calls to one plugin are serialized; different plugins and workflows run concurrently. The `Context` (`run_id`, `artifact_dir`, `user`, `iteration`) sets `user` and `iteration` to `null` outside a workflow.

Messages are one line of JSON each. A request carries `operation`, `inputs` and `context`. The reply repeats `id` and holds exactly one of `result` or `error`.

`$describe` returns the manifest within 10 seconds. `$shutdown` returns `{}` and the plugin exits within 1 second, or the host kills it.

- Each operation runs under its step's `timeout-ms` (default `60000`). Plugins reject protocol versions other than `1`.
- A message is capped at 8 MiB, including a line with no newline. A reply over the cap fails, so return large lists as artifacts.
- The host logs a `step-progress` event every five seconds per running step.
- An `Artifact` holds a version, a file path and a BLAKE2 hash. The file must stay in the run directory, reads are capped at 256 MiB, and the hash is checked. Give artifacts unique names when an operation runs more than once.
- Standard output carries replies only. Standard error goes to `plugins/<id>/stderr.log`, owner-only on Unix. Never write credentials to it.
- The host clears the environment and passes only `PATH`, `TMPDIR`, `TEMP`, `SYSTEMROOT`, `SSL_CERT_FILE` and `SSL_CERT_DIR`.

An operation error (including a failed assertion), a crash, a malformed reply, a cancellation or a timeout ends the plugin process. The host never restarts a plugin or retries an operation. Teardown runs the remaining steps, but a dead plugin cannot clean up. Deadlines include time waiting for the plugin's lock, and the whole-run and teardown deadlines bound shutdown. Secret input values are kept out of host events and errors.

> **Warning:** Plugins are trusted code and are not sandboxed, so review them as you would any code that handles your keys.

## Registry

The registry is a JSON file, `~/.config/polkameter/plugins.json` by default. Set `POLKAMETER_PLUGIN_REGISTRY` to use another path. It stores plugins, credential profiles, topology aliases and capabilities, and for a credential only the variable name.

| Command | Does |
| --- | --- |
| `polkameter plugin install EXECUTABLE` | Records the plugin's ID, version and executable hash. |
| `polkameter plugin list` | Prints the installed plugins. |
| `polkameter plugin inspect PLAN` | Checks operations against manifests. Prints manifests, requirements, targets and monitor requirements. |
| `polkameter plugin credential PROFILE ENV` | Maps a plan's credential profile to a variable on this host. |
| `polkameter plugin topology NAME PATH` | Registers a Zombienet `zombie.json` under an alias. |
| `polkameter plugin capability NAME` | Records a capability the network provisioner supplies. |

[CLI reference](cli.md#plugin) lists flags.

A plan names a credential profile, never the secret. The host reads the mapped variable at run start, and fails the run if it is unset or empty. A plugin gets a value only as input `credentials.ID`, and only in steps that reference it.

## Checksums

`plugin install` records the executable's BLAKE2-256 hash. `plugin inspect`, `preflight` and `run` compare the file on disk with it. A mismatch stops the run:

```text
$ polkameter preflight hello.polkameter.xml
Polkameter: plugin hello version/checksum mismatch
```

The command exits with code 3.

A `run` prints the same error as `Polkameter: run failed: <error> (details in …)`, writes it to `execution.json` and exits with `1`. Rebuilding changes the hash, so reinstall after every build. After start, the manifest must match the plan's `id`, `version` and `protocol`, or the run fails with `manifest identity mismatch for <plugin id>`.

## PreparedTx and the load contract

A `PreparedTx` is one transaction a plugin prepared before the load:

| Field | Type | Meaning |
| --- | --- | --- |
| `bytes` | hex string | The encoded transaction. `0x` is optional. |
| `hash` | hex string | The BLAKE2-256 hash of the bytes. |
| `metadata` | JSON | Anything the plugin needs later. Defaults to `null`. |

`PreparedTx::new(bytes, metadata)` computes the hash. `decode()` refuses empty bytes and a hash that does not match.

A `<load>` takes two `PreparedTx` lists, inline or as an `Artifact`: `source-ref` is the flood, and `probes-ref` holds the probes for baseline and recovery. The lists share no transaction, and each hash is sent at most once. The [probe budget](plans.md#load) sets how many probes to reserve, and exhausting the list is a stop reason. `<rate>` and `<ramp>` set the pace; [Plans](plans.md) covers `<thresholds>`.

With `state-check` and `state-ref`, PolkaMeter calls the named operation on a sample of finalized transactions. The operation receives `target`, `state`, `hashes` and `at`, where `hashes` and `at` are lowercase hex without `0x`. It returns `checked`, `missing` and `detail`.

`core.submit-prepared` waits for an RPC reply. `core.submit-setup` also waits for finalized inclusion, checks dispatch and Sudo results, and returns transaction hashes and the finalized block hash. After a submit times out, the transaction may still be accepted, so PolkaMeter never retries it.

Each submitted transaction gets exactly one status in `transactions.jsonl`: `finalized`, `in_pool`, `refused`, `lost`, `unverified` or `unknown`. A transaction seen in a best block without verified finality is `unverified`. Missing pool or finality evidence never makes a transaction `lost`.

Preflight measures the block interval before the load. If it differs from `block-interval-seconds` by more than 25%, the run stops before setup.

## Observers and checks

- **Observer.** A setup step starts it; an evaluate step stops it. It writes `series.jsonl` (same records as `chain.jsonl`) and `metrics.json` to the plugin's directory. `metrics.json` lists `PluginMetric` values, each with `name`, `kind`, `help` and `buckets`. A plugin cannot redefine a built-in metric.
- **Check.** An evaluate step whose operation declares a `checks` output contributes results. Each is an object with `outcome`, `check`, `status`, `detail`, and optional `numbers` and `optional`. `status` is `pass`, `warn`, `fail`, `info` or `no result`. Pass `run.directory` to the operation, and build `polkameter_checks::RunData` from `read_store` and `summary.json`. Smoke mode fails a check with no result unless it is `optional`.

Checks read the observer's series, labelled `job="plugin"` and `instance=<plugin id>`. Results go to `plugin-checks.json`. `polkameter report RUN_DIRECTORY` reproduces the summary offline.

## Library crates for plugins

These crates are not on crates.io. Depend on each from git at the SDK's `rev`, for example `polkameter-chain = { git = "https://github.com/agustinustheo/polkameter", rev = "<commit>" }`.

Anything else is internal.

| Crate | Plugin API |
| --- | --- |
| `polkameter-plugin-sdk` | `Plugin`, `serve`, `Operation`, `Schema`, `Manifest`, `Context`, `Artifact`, `PreparedTx`, `StateCheckInput`, `PROTOCOL`, `MAX_MESSAGE_BYTES`, `blake2_hex`, `strip_0x`. |
| `polkameter-chain` | `Client` (`connect`, `finalized`, `at`, `chain_info`, `check_extensions`, `call_data`, `sudo`, `nonce`, `validate`, `submit`; `LayoutChanged` from the extension check); `fetch(at, pallet, entry, keys)` (`()` keys for a plain entry), `entries`, `has_prefix`, `calls`; `events` with `Event` (`pallet`, `name`, `extrinsic`, `fields`); `value::{field, nth, variant_name, as_u64, as_bytes32}`; `Keypair`, `Value`, `DecodeAsType`, `ChainInfo`, `tx_hash`; `scale_value_reexport`, `scale_decode_reexport`. |
| `polkameter-monitors` | `ChainSeries` (`gauge`, `inc`, `observe`), `chain_series::channel`, `chain_series::run`; `walker::Walk` (`NAME`, `on_block`, optional `on_update` and `finish`), `walker::walk`; `MonitorError`. |
| `polkameter-files` | `registry::Metric::new`, `Def`, `Kind`, `PluginMetric::from(&Def)`, `Problems`, `RunDir`, `read_store`, `now_ms`, `Millis`, `num`. |
| `polkameter-checks` | `RunData` (`new`, `at`, `has`, `times`, `gauge_range`, `buckets`, `phase`, `run`); `Phase`; `Window`, `Label`; `CheckResult`, `Verdict`, `Status`, `CounterReset`, `quantile`. |

The load, monitor, check and file crates derive from the stress tool in [paritytech/polkadot-pop-e2e](https://github.com/paritytech/polkadot-pop-e2e), under Apache-2.0.
