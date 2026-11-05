# Troubleshooting

Find the message you see below. Each heading is the message as the CLI prints it, without the `PolkaMeter:` prefix. The v0.2.0 CLI spells the prefix `Polkameter:`. The entries that start with `error:` are printed by Cargo or by the argument parser.

## Building and installing

### error: rustc 1.92.0 is not supported by the following packages:

**Cause:** The default Rust toolchain is older than 1.93, which PolkaMeter requires. Cargo lists the packages that need a newer version.

**Fix:** Run `rustup update`. Or install the pinned toolchain with `rustup toolchain install 1.93.0`, then build with `cargo +1.93.0 build`.

<!-- v0.3.0: remove after release -->
### error: failed to select a version for the requirement `polkameter-plugin-sdk = "^0.3"`

```text
error: failed to select a version for the requirement `polkameter-plugin-sdk = "^0.3"`
candidate versions found which didn't match: 0.2.0
location searched: crates.io index
required by package `hello-plugin v0.1.0 (~/hello-plugin)`
```

**Cause:** The plugin SDK 0.3 is available from v0.3.0. crates.io has 0.2.0 only.

**Fix:** Depend on a commit of this repository until the release, as [Writing a plugin](writing-a-plugin.md#create-the-crate) shows.

<!-- v0.3.0: remove after release -->
### error: unexpected argument '--junit' found

```text
error: unexpected argument '--junit' found

  tip: to pass '--junit' as a value, use '-- --junit'

Usage: polkameter run --output <OUTPUT> <PLAN>

For more information, try '--help'.
```

**Cause:** `--junit` and `junit.xml` are available from v0.3.0. The published v0.2.0 CLI rejects the flag and exits with code `2`.

**Fix:** Drop `--junit`, or build the CLI from source with `cargo install --locked --git https://github.com/agustinustheo/polkameter polkameter`. Check the version with `polkameter --version`.

## Runs

### run failed: … (details in …/execution.json)

```text
Run failed. Artifacts: ~/polkameter/target/runs/run-…
PolkaMeter: run failed: assertion verified failed: expected 85, got 84 (details in ~/polkameter/target/runs/run-…/execution.json)
```

**Cause:** The run started, but a step, a plugin or a deadline failed it. The text after `run failed:` is the reason. Many reasons have their own entry on this page, such as [`assertion <id> failed`](#assertion--failed).

**Fix:** Find the reason on this page. `execution.json` in the run directory holds the full error. If a step failed, `events.jsonl` names it in a `step-finished` event with `"success":false`. A failure outside a step, such as a missing plugin or the plan's deadline, has no such event, so read `execution.json`.

## Plans

### line 6, column 5: expected </setup>, found </polkameter-plan>

**Cause:** The XML is not well formed. Here, `<setup>` is never closed.

**Fix:** Go to the line and column, and fix the element it names.

### missing `name` on <polkameter-plan>

**Cause:** A required attribute is missing from the root element. The root needs `version`, `xmlns` and `name`.

**Fix:** Add the attribute. Use `version="1"` and `xmlns="https://polkameter.dev/schema/plan"`.

### unknown element <stepz> in <polkameter-plan>

**Cause:** An element name is not in the plan schema.

**Fix:** Use the element names in `schemas/polkameter-plan.xsd` and in the [plan reference](plans.md).

### unknown or forward reference steps.nope.value

**Cause:** A `ref` names a step that does not exist, or a step that runs later.

**Fix:** Correct the step ID, or move the referenced step earlier.

### assertion <id> failed

**Cause:** A `core.assert-equal` step found that `actual` and `expected` differ. The run fails with exit `1`, and so does a CI job.

**Fix:** Correct `expected`, or fix the value that the step computes. The message names the step.

### cannot read … No such file or directory (os error 2)

```text
PolkaMeter: cannot read run directory target/no-such-run: No such file or directory (os error 2)
```

**Cause:** The path after `cannot read` does not exist. The message says which kind of file PolkaMeter wanted: `plan`, `plugin executable` or `run directory`.

**Fix:** Check the path. For `report`, pass the `run-…` directory itself, the one that holds `execution.json`.

## Plugins

### plugin <id> is not installed on this host

**Cause:** This host's plugin registry has no plugin with that ID. `validate` does not check installs, so it passes.

**Fix:** Run `polkameter plugin install <path-to-executable>` on this host. If you use a registry file, check that `POLKAMETER_PLUGIN_REGISTRY` points to it.

### plugin <id> version/checksum mismatch

**Cause:** The executable changed after `plugin install`, or the plan's `version` does not match the manifest.

**Fix:** Run `polkameter plugin install <path-to-executable>` after every rebuild. Match the plan's `version` to the manifest. `preflight` exits `3`, and `run` exits `1`.

### unknown operation example.nope

**Cause:** The operation is not in the installed plugin's manifest. `validate` does not check it.

**Fix:** Run `polkameter plugin inspect <plan>` to list the operations. Fix the name, or reinstall the plugin.

## Credentials and topologies

### credential profile sudo is not configured on this host

**Cause:** The plan names a credential profile that has no mapping on this host.

**Fix:** Run `polkameter plugin credential sudo <ENV_VAR>`.

### credential profile sudo is unavailable

**Cause:** The variable mapped to the profile is not set in the environment that runs `polkameter`.

**Fix:** Export the variable in that environment.

### empty credential profile sudo

**Cause:** The variable is set, but its value is empty.

**Fix:** Set it to a non-empty value.

### topology alias zombie is not installed on this host

**Cause:** `<monitors topology="zombie">` names an alias that this host has not registered. `preflight` exits `3`.

**Fix:** Run `polkameter plugin topology zombie <path/to/zombie.json>`.

## Chains

### block interval calibration failed during preflight

**Cause:** Preflight could not measure the block interval, usually because the target endpoint is unreachable. The cause is not printed.

**Fix:** Check the `<target endpoint="ws://…">` value, and that the node is running and reachable.

### measured block interval <measured>s differs from configured <configured>s by more than 25%; correct the plan before setup

**Cause:** The `block-interval-seconds` attribute on `<load>` differs from the measured block time by more than 25%. Preflight stops before setup.

**Fix:** Set `block-interval-seconds` on `<load>` to the measured value. The default is `6.0`.

### run failed: insufficient probe reserve

**Cause:** The output named by `probes-ref` holds fewer transactions than the run needs for baseline and recovery probes. With the defaults, the run needs 165.

**Fix:** Pass `ref="run.probeBudget"` as the plugin's probe count, as [Run a load test](load-test.md#write-a-plan) shows. [Probe budget](plans.md#load) gives the formula.

### run failed: duplicate transaction in flood/probe sources

**Cause:** The same transaction appears in both the flood and the probes, or twice in one list. Each transaction can be sent only once.

**Fix:** Prepare the probes as separate transactions, for example from a signer the flood does not use.

### The node refuses transactions

A node refuses a transaction when its pool will not accept it. The run does not stop on the first refusal. More than 1% refused marks a breaking point, and more than 10% in a step stops the load as `graceful`.

To see why, open these files in the run directory:

- `summary.md`: the `refusals are clean` check counts refusals by error code.
- `steps.jsonl`: each step's `errors` field counts refusals by code and message, as the node sent them.

These are the error codes of a Polkadot SDK transaction pool:

| Code | Meaning | Usual cause |
| --- | --- | --- |
| `1010` | Invalid transaction. The message says why, such as `Transaction is outdated` or `Inability to pay some fees`. | A stale nonce, a signer without funds, or a transaction built for another runtime version. |
| `1012` | Temporarily banned. | The same transaction was recently refused or dropped. |
| `1013` | Already imported. | The transaction was sent twice. |
| `1014` | Priority is too low. | Another transaction with the same signer and nonce is in the pool. |
| `1016` | Immediately dropped. | The pool is full. |

**Fix:** For `1010` and `1014`, rebuild the transactions against the current chain: fresh nonces, funded signers and the current runtime version. For `1016`, lower the rate or raise the node's pool limits. For any other code, read the node's log at the time of the refusals.

## Remote runs

### remote agent does not support --fail-on-verdict; update the agent

**Cause:** The client requested `--fail-on-verdict`, but the agent does not advertise support. The client stops before starting the run.

**Fix:** Update the agent to the same PolkaMeter ref as the client. Or omit the flag to use the agent's default exit policy.

### remote runner returned 401 Unauthorized: missing or invalid bearer token

**Cause:** The client's token does not match the agent's token.

**Fix:** Set the same value on both sides. The client reads the variable named by `--remote-token-env`. The agent reads the variable named by `--token-env`.

### remote runner returned 400 Bad Request: plugin <id> is not installed on this host

**Cause:** The agent's host has no plugin with that ID. The client cannot upload plugins.

**Fix:** Install the plugin on the agent host.

### remote runner returned 400 Bad Request: a plugin plan is already running

**Cause:** The agent runs one plan at a time.

**Fix:** Wait for the run to end, or press Ctrl-C in the client that started it.

### remote runner endpoint must use https://, or http:// only through a loopback SSH tunnel

**Cause:** `--remote` uses plain `http://` with a host that is not loopback. The client refuses it before sending anything.

**Fix:** Use `https://`. Or tunnel to the agent and use `http://127.0.0.1:<port>`.

### agent binds only to a loopback address; use an SSH tunnel or TLS terminator

**Cause:** `agent serve --bind` names a non-loopback address.

**Fix:** Bind to `127.0.0.1:<port>`, and reach the agent through an SSH tunnel.
