# CLI reference

`polkameter` checks and runs plans, rebuilds reports and manages plugins. Use `polkameter COMMAND --help` for a command's options and examples. `polkameter --version` prints the installed version. For error messages, see [Troubleshooting](troubleshooting.md).

## init

Writes a load plan for the [substrate plugin](substrate-plugin.md) from a live chain. It checks that the runtime has the calls the plan needs and measures the block time. [Generate a plan](substrate-plugin.md#generate-a-plan) shows the result.

```sh
polkameter init --rpc ENDPOINT [--kind remarks|transfers] [--profile PROFILE] [--output PATH] [--force]
```

`--kind remarks`, the default, loads the chain with `System.remark` calls. `--kind transfers` uses `Balances.transfer_keep_alive`. `--profile` names the [credential profile](#plugin) for the signers' seed, `substrate-dev` by default. Without `--output`, the plan goes to standard output. An unreachable chain, or a runtime without a needed call, exits `3`. An existing `--output` file without `--force` exits `2`.

## validate

Checks a plan's XML, structure, built-in `core.` operations and references. It does not start plugins or connect to a chain. An invalid plan exits `2` and names the file, line and column of the first problem.

```sh
polkameter validate examples/plugin-data-flow.polkameter.xml
```

```text
Plan is structurally valid. Preflight checks the installed plugins.
```

## preflight

Starts the plan's plugins and checks the plan's operations and inputs against them. It also checks that every credential's variable is set. Then it checks the chain and runs the read-only `<preflight>` steps. For a load test, it also measures the block interval. It submits nothing. A failed check exits `3`. Install the plugins first with [`plugin install`](#plugin).

```sh
polkameter preflight examples/plugin-data-flow.polkameter.xml
```

```text
Preflight passed.
```

## run

Runs a plan. It writes one `run-*` directory under `--output`.

```sh
polkameter run PLAN --output DIR [--fail-on POLICY] [--credential-env ENV_VAR] [--shutdown-grace SECONDS]
polkameter run examples/plugin-data-flow.polkameter.xml --output runs
```

```text
Phase: preflight
...
Run completed. Artifacts: ~/polkameter/runs/run-1792255021142-12303-1
```

Phase lines go to standard error. The `Run …` line goes to standard output and names the run directory. `--credential-env` is for a plan with one credential. It reads that credential from the given variable instead of its profile. `preflight` takes it too.

If the run fails, PolkaMeter prints the reason on standard error, and `execution.json` keeps the full text. A run that fails before setup submitted nothing. It prints `run failed before setup:`, skips teardown and exits `3`.

By default, a `FAIL` or `INCONCLUSIVE` [verdict](reading-results.md#did-it-pass) exits `1`. `--fail-on verdict` exits `1` only on `FAIL`. `--fail-on error` ignores the verdict, so only a failed run exits `1`. `WARN` never changes the exit code. After the report and teardown, a line on standard error names the reason:

```text
PolkaMeter: verdict INCONCLUSIVE: required checks with no result: no silent loss; exit 1 ...
```

In [smoke mode](plans.md#polkameter-plan), a monitor problem or a required check with no result always exits `1`. `execution.json` keeps the reason line as [`exit_reason`](results.md#executionjson).

### Stop a run

Press Ctrl-C (SIGINT) or send SIGTERM, as CI systems do when they cancel a job. PolkaMeter stops the running step and runs the plan's teardown. Then it writes the results, stops the plugins and exits `130` for SIGINT or `143` for SIGTERM. The run's state is `stopped`.

```text
Phase: preflight
Phase: setup
PolkaMeter: received SIGTERM; stopping the run (teardown has up to 20 s; send the signal again to stop now)
Phase: teardown
Run stopped. Artifacts: ~/polkameter/runs/run-1793933833255-39334-1
```

Teardown after a signal gets `--shutdown-grace` seconds, 20 by default. A second signal, or the end of that period, interrupts teardown. The `teardown` field in `execution.json` is then `interrupted` instead of `completed`. If the process is killed before it writes the outcome, `state` stays `stopping`. So exit code `130` or `143` alone doesn't prove that teardown ran.

PolkaMeter then needs up to 10 more seconds to write the outcome. If your CI system sends SIGKILL a set time after SIGTERM, set `--shutdown-grace` 10 seconds below that time.

> **Warning:** SIGKILL cannot be handled. In a container, start PolkaMeter with an init process, such as `docker run --init`, so that signals reach it.

## report

Rebuilds a run's reports from its files, without running the plan. See [Rebuilding a report](results.md#rebuilding-a-report).

```sh
polkameter report runs/run-1792255021142-12303-1
```

`report` exits `0` when the rebuild succeeds, even if the run failed. `state` in `execution.json` says how the run ended. If `summary.json` has no `schemaVersion`, or one other than `1`, `report` changes no file and exits `2`.

## plugin

Installs plugins and records this host's credential profiles and topologies. A plan refers to them by name. This host's registry, a JSON file, says what each name points to. [Environment variables](#environment-variables) gives its path. A failed `plugin` subcommand exits `2`. `plugin list` and `plugin inspect` print JSON.

| Command | Effect |
| --- | --- |
| `plugin install EXECUTABLE` | Records the plugin's ID, version and BLAKE2 checksum |
| `plugin list` | Prints the registry. Credentials show variable names, never values |
| `plugin inspect PLAN` | Starts the plan's plugins and prints their manifests. It submits nothing |
| `plugin credential PROFILE ENV_VAR` | Maps a credential profile to an environment variable |
| `plugin topology NAME PATH` | Registers the `zombie.json` of a Zombienet test network as NAME, for `<monitors topology="NAME">` |

Install a plugin again after each rebuild. PolkaMeter refuses to run a plugin whose file changed.

```sh
polkameter plugin install target/debug/polkameter-example-plugin
```

```text
Installed plugin example 0.1.0 (7 operations) from ~/polkameter/target/debug/polkameter-example-plugin
```

A credential profile maps a plan's credential name to an environment variable on the run host. The registry stores the variable's name, not its value. PolkaMeter reads the variable when a run starts. An unmapped profile, or an unset or empty variable, makes `preflight` and `run` exit `3`.

## Exit codes

[Did it pass?](reading-results.md#did-it-pass) says how the verdict relates to the exit code.

| Code | Meaning |
| --- | --- |
| `0` | The command succeeded, or the run ended `completed`. |
| `1` | The run failed in setup or later. Or it ended `completed_with_failures` or `completed_inconclusive`, and `--fail-on` or smoke mode makes that an error. Or `report`, or writing the output, failed. |
| `2` | Invalid input or usage: an invalid plan, a bad flag, a corrupt registry, a failed `plugin` subcommand, or a `summary.json` that `report` cannot rewrite. |
| `3` | Preflight checks failed, or `init` could not prepare the plan. A `run` that exits `3` submitted nothing and skipped teardown. |
| `130` | Stopped by SIGINT. |
| `143` | Stopped by SIGTERM. |

Run `polkameter preflight <plan>` to find exit-`3` problems without a run directory.

## Environment variables

| Variable | Effect |
| --- | --- |
| `POLKAMETER_PLUGIN_REGISTRY` | Path of the plugin registry. Default: `~/.config/polkameter/plugins.json`. |
| `POLKAMETER_NODE_PID` | The node's process ID, for CPU and memory in a load test. See [node: … no CPU or memory recorded](troubleshooting.md#node--no-cpu-or-memory-recorded). |

Plugins start with a cleared environment. Only `PATH`, `TMPDIR`, `TEMP`, `SYSTEMROOT`, `SSL_CERT_FILE` and `SSL_CERT_DIR` pass through. A plugin gets credential values only as step inputs.
