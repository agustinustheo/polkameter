# Plugin reference

A plugin is a separate executable that supplies operations the plan can call. The plan calls the plugin's operations by name. PolkaMeter sends requests through standard input and reads replies from standard output. [Writing a plugin](writing-a-plugin.md) builds one.

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
| `id` | string | required | The plugin's name in plans. Not empty or `core` |
| `version` | string | required | Must equal the plan's `<plugin>` `version` |
| `protocol` | integer | required | `1`. Must match the plan's `protocol` |
| `operations` | map of name to operation | required | The operations a plan can call |
| `requirements` | list | `[]` | Evidence checked before setup |

Each operation has these fields:

| Field | Type | Default | Meaning |
| --- | --- | --- | --- |
| `description` | string | required | Shown by `plugin inspect` |
| `inputs` | map of field | required | Inputs the operation accepts |
| `outputs` | map of field | required | Outputs the operation must return |
| `read_only` | boolean | `false` | `true` if it changes nothing on a chain and submits nothing. Only these run in `<preflight>` |

Each input and output has a `schema` and an `optional` flag. `optional` defaults to `false`.

A requirement names evidence the host checks before setup:

| Field | Type | Default | Meaning |
| --- | --- | --- | --- |
| `kind` | string | required | `rpc`, `metric` or `capability` |
| `target` | string | required | `rpc`: target ID in the plan. `metric`: monitor role. Unused by `capability` |
| `name` | string | required | `rpc`: RPC method. `metric`: metric family. `capability`: capability name |
| `required` | boolean | required | Optional requirements are not checked |

An `rpc` requirement passes when the target node lists the method in `rpc_methods`. A `metric` requirement passes when every node of the role exposes the family. A `capability` requirement passes when [`polkameter plugin capability`](cli.md#plugin) recorded the name.

### Schemas

Each `schema` has a `type` key:

| Type | Accepts |
| --- | --- |
| `string` | JSON string |
| `number` | JSON number |
| `integer` | JSON integer |
| `boolean` | JSON boolean |
| `array` | Array whose items match `items` |
| `object` | The declared `fields`. An unknown field fails. A missing field fails unless it is `optional` |
| `json` | Any value |

The host checks inputs before the call and outputs when the plugin replies. A mismatch becomes an error:

```json
{"protocol":1,"id":3,"result":null,"error":"value does not match String"}
```

[References](plans.md#references) lists the values a step can pass between steps.

## Protocol

The plugin protocol exchanges one JSON message per line over standard input and output. A request carries `operation`, `inputs` and `context`. The reply repeats the request `id` and contains exactly one of `result` or `error`.

PolkaMeter starts one process per plugin per run and keeps it until the run ends. Users in a workflow can run concurrently. Workflows run in plan order, and calls to each plugin are serialized. The `Context` holds `run_id`, `artifact_dir`, `user` and `iteration`. `user` and `iteration` are `null` outside a workflow.

`$describe` must return the manifest within 10 seconds. `$shutdown` returns `{}`. The plugin must then exit within 1 second, or the host kills it.

- Each operation runs under its step's `timeout-ms`.
- Keep each message within 8 MiB. Return an artifact reference for larger data.
- An `Artifact` holds a version, a file path in the run directory and a BLAKE2 hash. The host checks the hash and reads at most 256 MiB. Give artifacts unique names when an operation runs more than once.
- Standard output carries replies only. Standard error goes to `plugins/<id>/stderr.log`. The host [redacts](results.md#redaction) credentials from it as it is written. Never write credentials to it anyway.
- The host clears the environment. It passes only `PATH`, `TMPDIR`, `TEMP`, `SYSTEMROOT`, `SSL_CERT_FILE` and `SSL_CERT_DIR`.

An `error` reply fails the step, but the plugin keeps running. A crash, a malformed reply, a cancellation or a timeout ends the plugin process, and later steps of that phase fail. Before each teardown step, the host starts an ended plugin again and logs a `plugin-restarted` event. The host never retries an operation.

When the host kills a plugin, it also kills every process the plugin started.

> **Warning:** Plugins run as trusted code. They are not sandboxed. Review a plugin as you would any code that handles your keys.

## Registry

The registry is a JSON file, `~/.config/polkameter/plugins.json` by default. Set `POLKAMETER_PLUGIN_REGISTRY` to use another path. It stores plugins, credential profiles, topology aliases and capabilities. For a credential profile it stores only the variable name. [`polkameter plugin`](cli.md#plugin) manages it.

`plugin install` records the executable's hash. Install the plugin again after a rebuild.

A plan names a credential profile, never the secret. The host reads the mapped variable at run start. The run fails if the variable is unset or empty. A plugin gets the value only as the input `credentials.ID`, and only in steps that reference it.

## PreparedTx and the load contract

A `PreparedTx` is one transaction a plugin prepared before the load:

| Field | Type | Meaning |
| --- | --- | --- |
| `bytes` | hex string | The encoded transaction. `0x` is optional |
| `hash` | hex string | The BLAKE2-256 hash of the bytes |
| `metadata` | JSON | Anything the plugin needs later. Defaults to `null` |

`PreparedTx::new(bytes, metadata)` computes the hash. `decode()` refuses empty bytes and a hash that does not match.

A `<load>` takes two `PreparedTx` lists, inline or as an `Artifact`. `source-ref` holds the flood transactions. `probes-ref` holds the probe transactions. Use separate prepared transactions for the flood and probes. The [probe budget](plans.md#load) sets how many probes to reserve. If the probes run out, the load stops.

With `state-check` and `state-ref`, PolkaMeter calls the named operation on a sample of finalized flood transactions. The operation gets `target` (the WebSocket endpoint), `state`, `hashes` and `at`. `hashes` and `at` are lowercase hex without `0x`. It returns `checked`, `missing` and `detail`.

[Loss accounting](results.md#loss-accounting) gives each sent flood transaction one final status. Funding transactions and probes are separate.

## Observers and checks

An observer records extra series during the load. A setup step starts it and an evaluate step stops it. It writes `series.jsonl` and `metrics.json` to the plugin's directory. `metrics.json` lists `PluginMetric` values. A plugin cannot redefine a built-in metric.

A check adds results to the report. Any evaluate step whose operation declares a `checks` output is a check. Each result has `outcome`, `check`, `status`, `detail`, and optional `numbers` and `optional`. `status` is `pass`, `warn`, `fail`, `info` or `no result`. To read the run's data, pass `run.directory` to the operation and build `RunData` from `read_store` and `summary.json`. In smoke mode, a check with no result fails unless it is `optional`.

Checks read the observer's series with the labels `job="plugin"` and `instance=<plugin id>`. Results go to `plugin-checks.json`. `polkameter report RUN_DIRECTORY` rebuilds the summary offline.

## The plugin SDK crate

A plugin depends on one crate, `polkameter-plugin-sdk` ([crates.io](https://crates.io/crates/polkameter-plugin-sdk), [API documentation](https://docs.rs/polkameter-plugin-sdk/0.3)).

```toml
[dependencies]
polkameter-plugin-sdk = { version = "0.3", features = ["observers"] }
```

The default features give the plugin protocol: `Plugin`, `serve`, the manifest types, `Artifact` and `PreparedTx`. The default build does not depend on subxt, so it builds faster.

The `chain` feature reads runtime metadata and storage, and signs and submits transactions for supported account types and transaction extensions.

The `observers` feature records series, reads run data and builds check results. It turns on `chain` too.

Hidden items may change in any release. The `polkameter` crate is the CLI, not a library for plugins.

The chain, monitor, check and file code derives from the stress tool in [paritytech/polkadot-pop-e2e](https://github.com/paritytech/polkadot-pop-e2e), under Apache-2.0.

### Migrating from 0.2

Version 0.3 merges the old plugin crates into `polkameter-plugin-sdk`. Replace the old dependencies with the line above. Use `features = ["chain"]` if the plugin does not use `polkameter-files`, `polkameter-checks` or `polkameter-monitors`. Then change the import paths:

| 0.2 | 0.3 |
| --- | --- |
| `polkameter_plugin_sdk::*` | `polkameter_plugin_sdk::*` (unchanged) |
| `polkameter_chain::*` | `polkameter_plugin_sdk::chain::*` |
| `polkameter_chain::value::*` | `polkameter_plugin_sdk::chain::value::*` |
| `#[decode_as_type(crate_path = "polkameter_chain::scale_decode_reexport")]` | `#[decode_as_type(crate_path = "polkameter_plugin_sdk::chain::scale_decode_reexport")]` |
| `polkameter_files::*` | `polkameter_plugin_sdk::observers::*` |
| `polkameter_files::registry::*`, `polkameter_files::summary::*` | `polkameter_plugin_sdk::observers::registry::*`, `polkameter_plugin_sdk::observers::summary::*` |
| `polkameter_checks::*` | `polkameter_plugin_sdk::observers::*` |
| `polkameter_monitors::{ChainSeries, MonitorError}` | `polkameter_plugin_sdk::observers::{ChainSeries, MonitorError}` |
| `polkameter_monitors::walker::*`, `polkameter_monitors::chain_series::*` | `polkameter_plugin_sdk::observers::walker::*`, `polkameter_plugin_sdk::observers::chain_series::*` |

`polkameter-engine` and `polkameter-load` have no replacement. They are part of the `polkameter` CLI.

`CheckResult` has a new field, `source`. PolkaMeter sets it to the evaluate step's ID, so a plugin that builds a `CheckResult` sets `source: None`.
