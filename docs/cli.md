# CLI reference

`polkameter` checks and runs plans, rebuilds reports, manages plugins and serves a remote agent. Use `polkameter COMMAND --help` for a command's options and examples. `polkameter --version` prints the installed version. For error messages, see [Troubleshooting](troubleshooting.md).

## init

Writes a load plan for the [substrate plugin](substrate-plugin.md) from a live chain. It checks that the runtime has the calls the plan needs and measures the block time. [Generate a plan](substrate-plugin.md#generate-a-plan) shows the result.

```sh
polkameter init --rpc ENDPOINT [--kind remarks|transfers] [--profile PROFILE] [--output PATH] [--force]
```

`--kind remarks` loads the chain with `System.remark` calls, and `--kind transfers` with `Balances.transfer_keep_alive`. Without `--output`, the plan goes to standard output. An unreachable chain, or a runtime without a needed call, exits `3`. An existing `--output` file without `--force` exits `2`.

## validate

Checks a plan's XML, structure, built-in `core.` operations and references. It does not start plugins or connect to a chain. An invalid plan exits `2` and names the file, line and column of the first problem.

```sh
polkameter validate examples/plugin-workflow.polkameter.xml
```

```text
Plan is structurally valid. Preflight checks the installed plugins.
```

## preflight

Starts the plan's plugins and checks each operation and input against its plugin's manifest. It checks that every credential the plan declares has its variable set. It also checks the chain, runs the read-only `<preflight>` steps and, for a plan with a load, measures the block interval. It submits nothing. A failed check exits `3`. Install the plugins first with [`plugin install`](#plugin).

```sh
polkameter preflight examples/plugin-workflow.polkameter.xml
```

```text
Preflight passed.
```

## run

Runs a plan. A local run writes one `run-*` directory under `--output`. A remote run sends the plan to an agent, which keeps the run directory. See [Run on a remote machine](remote-runs.md).

```sh
polkameter run examples/plugin-workflow.polkameter.xml --output runs
```

```text
Phase: preflight
...
Run completed. Artifacts: ~/polkameter/runs/run-1792255021142-12303-1
```

Phase lines go to standard error. The last line goes to standard output and names the run directory. `--junit PATH` writes an extra copy of `junit.xml`, also for a remote run.

If the run fails, PolkaMeter prints the reason on standard error and keeps the run directory. `execution.json` holds the full text. Here, a plugin process exited during a step:

```text
Phase: preflight
Phase: setup
Phase: teardown
Run failed. Artifacts: ~/polkameter/runs/run-1792994214466-72728-1
PolkaMeter: run failed: plugin exited before responding (details in ~/polkameter/runs/run-1792994214466-72728-1/execution.json)
```

A run that fails before its setup phase submitted nothing. It prints `run failed before setup:`, skips teardown and exits `3`:

```text
Phase: preflight
Run failed. Artifacts: ~/polkameter/runs/run-1793948741238-14878-1
PolkaMeter: run failed before setup: plan.polkameter.xml: line 2, column 16: credential `sudo` (profile `sudo`) reads environment variable SUDO_SEED_X (from --credential-env), which is not set; set SUDO_SEED_X on the step that runs polkameter, or pass the variable that holds it to --credential-env (details in ~/polkameter/runs/run-1793948741238-14878-1/execution.json)
```

By default, the [verdict](reading-results.md#did-it-pass) of a stress-mode load test does not change the exit code. Add `--fail-on-verdict` to exit `1` on `FAIL`. Add `--fail-on-inconclusive` to exit `1` on `INCONCLUSIVE`. `WARN` never changes the exit code. Both flags act after the report and teardown, and a line on standard error names the reason:

```text
PolkaMeter: verdict INCONCLUSIVE: required checks with no result: no silent loss; exit 1 because of --fail-on-inconclusive (the checks are in summary.md)
```

In smoke mode, a monitor problem or a required check with no result exits `1`, whatever these flags say. Other verdict failures still follow the flags. `execution.json` keeps the reason line as [`exit_reason`](results.md#executionjson). With `--remote`, the agent applies both flags.

### Stop a run

Press Ctrl-C (SIGINT) or send SIGTERM, as CI systems do when they cancel a job. PolkaMeter stops the running step and runs the plan's teardown. Then it writes the results, stops the plugins and exits `130` for SIGINT or `143` for SIGTERM. The run's state is `stopped`.

```text
Phase: preflight
Phase: setup
PolkaMeter: received SIGTERM; stopping the run (teardown has up to 20 s; send the signal again to stop now)
Phase: teardown
Run stopped. Artifacts: ~/polkameter/runs/run-1793933833255-39334-1
```

Teardown after a signal gets `--shutdown-grace` seconds, 20 by default. A second signal, or the end of that period, interrupts teardown. The `teardown` field in `execution.json` is then `interrupted` instead of `completed`. If the process is killed before it writes the outcome, `state` stays `stopping`. So exit code `130` or `143` alone does not prove that teardown ran.

After a forced stop, PolkaMeter waits at most 10 seconds for the outcome. Set `--shutdown-grace` to the time between SIGTERM and SIGKILL, minus 10 seconds:

| Where | SIGTERM to SIGKILL | `--shutdown-grace` |
| --- | --- | --- |
| Kubernetes (`terminationGracePeriodSeconds`) | 30 s by default | `20` (the default) |
| GitLab Runner | Depends on the executor | That time minus 10 |
| `docker stop` | 10 s by default | `0`, or raise `docker stop --timeout` |
| GitHub Actions cancellation | SIGINT, SIGTERM 7.5 s later, SIGKILL 2.5 s after that | The SIGTERM interrupts teardown |

> **Warning:** SIGKILL cannot be handled. In a container, run PolkaMeter under an init process (`docker run --init`), so that signals reach it.

## report

Rebuilds a run's checks, `summary.json`, `summary.md`, `junit.xml`, `samples.jtl` and plots from its files. It does not run the plan. See [Rebuilding a report](results.md#rebuilding-a-report).

```sh
polkameter report runs/run-1792255021142-12303-1
```

```text
# Plugin data flow

Status: completed
...
```

`report` exits `0` when the rebuild succeeds, even if the run failed. Read the `Status:` line or the `state` in `execution.json`. If `summary.json` has no `schemaVersion`, or one other than `1`, `report` changes no file and exits `2`.

## plugin

Installs plugins and records this host's credential profiles, topologies and capabilities. A plan names these things. The host holds the values. The registry is a JSON file; [Environment variables](#environment-variables) gives its path. A failed `plugin` subcommand exits `2`. `plugin install` prints one line. The other subcommands print JSON.

| Command | Effect |
| --- | --- |
| `plugin install EXECUTABLE` | Records the plugin's ID, version and BLAKE2 checksum |
| `plugin list` | Prints the registry. Credentials show variable names, never values |
| `plugin inspect PLAN` | Starts the plan's plugins and prints their manifests. It submits nothing |
| `plugin credential PROFILE ENV_VAR` | Maps a credential profile to an environment variable |
| `plugin topology NAME PATH` | Registers a Zombienet `zombie.json` under the alias that `<monitors topology="…">` names |
| `plugin capability NAME` | Records a capability that the network provisioner supplies |

Install a plugin again after each rebuild. PolkaMeter refuses to run a plugin whose file changed.

```sh
polkameter plugin install target/debug/polkameter-example-plugin
```

```text
Installed plugin example 0.1.0 (7 operations) from ~/polkameter/target/debug/polkameter-example-plugin
```

A credential profile maps a plan's credential name to an environment variable on the run host. The registry stores the variable's name, not its value. PolkaMeter reads the variable when a run starts. An unmapped profile, or an unset or empty variable, makes `preflight` and `run` exit `3`.

```sh
polkameter plugin credential sudo SUDO_SEED
polkameter plugin list
```

```json
{
  "capabilities": [],
  "credentials": {
    "sudo": "SUDO_SEED"
  },
  "plugins": {
    "example": {
      "blake2": "2167d642faa16293c70c23a1aa10faef6b570aad31f24184d40d41727614e789",
      ...
```

## agent serve

Starts the remote agent on this host. The agent runs plans that clients send with `polkameter run --remote`. It uses its own plugins, credentials and topologies. It listens only on a loopback address and does not serve TLS. [Run on a remote machine](remote-runs.md) shows the setup.

```sh
POLKAMETER_AGENT_TOKEN=$(openssl rand -hex 32) polkameter agent serve --bind 127.0.0.1:9901
```

## Exit codes

The exit code tells the calling process how the command ended. Code `0` alone does not mean a `PASS` verdict.

| Code | Meaning |
| --- | --- |
| `0` | The command succeeded, or the run ended `completed`. |
| `1` | The run failed in setup or later, or ended `completed_with_failures`. A selected verdict flag, a smoke-mode check, `report`, a remote request or writing the output also failed. |
| `2` | Invalid input or usage: an invalid plan, a bad flag, a corrupt registry, a failed `plugin` subcommand, or a `summary.json` that `report` cannot rewrite. |
| `3` | Preflight checks failed, or `init` could not prepare the plan. A `run` that exits `3` submitted nothing and skipped teardown. |
| `130` | Stopped by SIGINT. |
| `143` | Stopped by SIGTERM. |

`agent serve` also exits `130` or `143` after a signal. A stopped run usually ran its teardown, but check [`teardown`](#stop-a-run) in `execution.json`. Run `polkameter preflight <plan>` to find exit-`3` problems without a run directory.

## Environment variables

| Variable | Effect |
| --- | --- |
| `POLKAMETER_PLUGIN_REGISTRY` | Path of the plugin registry. Default: `~/.config/polkameter/plugins.json`. `USERPROFILE` replaces `HOME` when `HOME` is not set. |
| `POLKAMETER_NODE_PID` | The node's process ID, for CPU and memory in a load test. See [Node CPU and memory](how-it-works.md#node-cpu-and-memory). |
| `POLKAMETER_AGENT_TOKEN` | The agent's bearer token when `agent serve` has no `--token-env`. |

Plugins start with a cleared environment. Only `PATH`, `TMPDIR`, `TEMP`, `SYSTEMROOT`, `SSL_CERT_FILE` and `SSL_CERT_DIR` pass through. A plugin gets credential values only as step inputs.

## Output formats

With `--format json`, `validate`, `preflight`, `run` and `report` print one JSON object per line on standard output. They do this also when they fail.

```sh
polkameter validate examples/plugin-workflow.polkameter.xml --format json
```

```json
{"event":"validation","plan":"Plugin data flow","schemaVersion":1,"valid":true}
```

An error goes to standard error, as in human mode, and an `error` line goes to standard output. The exit code does not change:

```json
{"column":34,"command":"validate","event":"error","exitCode":2,"file":"ref.xml","kind":"invalid","line":3,"message":"ref.xml: line 3, column 34: step `a` (core.echo): input `value` refers to `steps.nope.value`, but nothing in the plan defines it; …","schemaVersion":1,"valid":false}
```

### JSON output contract

Every line has `event` and `schemaVersion`. `schemaVersion` is `1`. It changes only when a field is removed or renamed, or its meaning changes. New fields and events can appear without a new version.

| `event` | Printed by | Other fields |
| --- | --- | --- |
| `validation` | `validate`, last line | `valid` (`true`), `plan` (the plan's `name`) |
| `preflight` | `preflight`, last line | `valid`, `plugins` (manifests), `requirements`, `targets`, `monitorRequirements`, `metricCatalog`, and `calibration` for a plan with a load |
| `phase`, `step-started`, `step-finished`, `warning`, `completed`, … | a local `run` | The events of [events.jsonl](results.md#eventsjsonl) |
| `artifact-written` | `run`, last line | `outcome`: the content of `execution.json`. A plan error also adds `file`, `line` and `column` |
| `report` | `report`, last line | `outcome` and `summary` (the text of `summary.md`) |
| `error` | any of the four, when it fails before it has a result | `command`, `kind` (`invalid`, `preflight` or `runtime`), `exitCode`, `message`. A plan error also adds `file`, and `line` and `column` when known. `validate` sets `valid` to `false` |

Keys that PolkaMeter adds are camelCase. `outcome` keeps the snake_case keys of [execution.json](results.md#executionjson). The plugin manifests keep the plugin protocol's keys, such as `read_only`.

Remote runs do not stream the agent's events. They print `artifact-written`, and a failed local JUnit download can also print a `warning`. Load runs also print `stop:`, `loss check:` and `smoke:` lines and the summary to standard error. Argument errors, such as an unknown flag, print as text and exit `2` before any JSON.
