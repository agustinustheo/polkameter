# CLI reference

<!-- v0.3.0: remove after release -->
> **Unreleased:** This page describes v0.3.0. The published CLI is v0.2.0, which has no `--junit`, writes no `junit.xml` and has no `init`. To use them now, build from source with `cargo install --locked --git https://github.com/agustinustheo/polkameter --rev 58921b7d506f9987db68fc8084b2fb8f13649f94 polkameter`.

`polkameter` validates, preflights and runs plans, reports on runs, manages plugins and serves a remote agent. Run `polkameter COMMAND --help` for one command's flags. Errors: see [Troubleshooting](troubleshooting.md).

## --version

Prints `polkameter <version>` and exits 0. `<version>` is the version of the build you installed. `-V` is the short form.

```sh
polkameter --version
```

```text
polkameter <version>
```

## init

Writes a load plan for the [substrate plugin](substrate-plugin.md) from a live chain. It checks that the runtime has the calls the plugin needs and measures the block time. [Generate a plan](substrate-plugin.md#generate-a-plan) shows the plan it writes.

```sh
polkameter init --rpc ENDPOINT [--kind remarks|transfers] [--profile PROFILE] [--output PATH] [--force]
```

| Name | Type | Required | Default | Limits | Meaning |
| --- | --- | --- | --- | --- | --- |
| `--rpc` | URI | yes | none | starts with `ws://` or `wss://` | The chain's endpoint |
| `--kind` | enum | no | `remarks` | `remarks` or `transfers` | The call the plan loads the chain with |
| `--profile` | string | no | `substrate-dev` | not empty | The credential profile the plan names for the seed |
| `--output` | path | no | standard output | | The plan file. `init` creates its directory before it contacts the chain |
| `--force` | flag | no | off | | Replace an existing `--output` file |

An unreachable chain exits with code `3`. An existing `--output` file without `--force` exits with code `2`.

## validate

Checks a plan's structure, and the built-in `core.` operations with their inputs, without starting plugins or connecting to a chain. [Plans](plans.md#validation-rules) lists the rules. An invalid plan exits `2` with the file, line and column of the first problem, as in `PolkaMeter: plan.polkameter.xml: line 3, column 34: …`.

```sh
polkameter validate [--format human|json] PLAN
```

| Name | Type | Required | Default | Limits | Meaning |
| --- | --- | --- | --- | --- | --- |
| `PLAN` | path | yes | none | a file or pipe of at most 2 MiB | The plan file |
| `--format` | enum | no | `human` | `human` or `json` | See [output formats](#output-formats) |

From the repository root:

```sh
polkameter validate examples/plugin-workflow.polkameter.xml
```

```text
Plan is structurally valid. Preflight checks the installed plugins.
```

## preflight

Checks each operation against its plugin's manifest and the evidence the plugins require, and that every credential the plan declares has its variable set. It also checks the chain and runs the read-only preflight steps, submitting nothing. A plan with a load also measures the block interval. Install plugins first: [`plugin install`](#plugin-install).

```sh
polkameter preflight [--credential-env ENV_VAR] [--format human|json] PLAN
```

| Name | Type | Required | Default | Limits | Meaning |
| --- | --- | --- | --- | --- | --- |
| `PLAN` | path | yes | none | a file or pipe of at most 2 MiB | The plan file |
| `--credential-env` | variable name | no | none | the plan declares exactly one credential | Reads the only credential from this variable, instead of its configured profile |
| `--format` | enum | no | `human` | `human` or `json` | See [output formats](#output-formats) |

```sh
polkameter preflight examples/plugin-workflow.polkameter.xml
```

```text
Preflight passed.
```

## run

Runs a plan. A local run writes a run directory under `--output`. A remote run sends the plan's XML to an agent, which runs it and keeps the run directory there.

```sh
polkameter run [--output DIR] [--junit PATH] [--credential-env ENV_VAR] [--fail-on-verdict] [--fail-on-inconclusive] [--shutdown-grace SECONDS] [--format human|json] PLAN
polkameter run --remote URL --remote-token-env ENV_VAR [--junit PATH] [--fail-on-verdict] [--fail-on-inconclusive] [--shutdown-grace SECONDS] [--format human|json] PLAN
```

| Name | Type | Required | Default | Limits | Meaning |
| --- | --- | --- | --- | --- | --- |
| `PLAN` | path | yes | none | a file or pipe of at most 2 MiB | The plan file |
| `--output DIR` | path | yes, unless `--remote` | none | a UTF-8 path | The artifact root. The run creates one `run-*` directory in it |
| `--junit PATH` | path | no | none | <!-- v0.3.0: remove after release -->from v0.3.0 | A local copy of the run directory's `junit.xml`, including for a remote run. Parent directories are created |
| `--credential-env` | variable name | no | none | not with `--remote`; the plan declares exactly one credential | Reads the only credential from this variable |
| `--fail-on-verdict` | flag | no | off | none | Exit `1` on a `FAIL` load verdict, after writing the report and running teardown. Other verdicts and plans without a load test keep their exit code |
| `--fail-on-inconclusive` | flag | no | off | <!-- v0.3.0: remove after release -->from v0.3.0 | Exit `1` on an `INCONCLUSIVE` load verdict, after writing the report and running teardown. Add `--fail-on-verdict` to also exit `1` on `FAIL` |
| `--remote URL` | URL | no | none | `https://`, or `http://` to a loopback address with a port | The agent's endpoint. See [remote agent](#remote-agent) |
| `--remote-token-env` | variable name | with `--remote` | none | needs `--remote` | The variable that holds the agent's bearer token |
| `--shutdown-grace SECONDS` | seconds | no | `20` | none | How long a stopping run gets to finish teardown after SIGINT or SIGTERM. See [stop a run](#stop-a-run) |
| `--format` | enum | no | `human` | `human` or `json` | See [output formats](#output-formats) |

When a verdict makes the run exit `1`, a line on standard error says why, after teardown, once the run ended `completed_with_failures`. It names the stop rule, or the checks that failed or have no result:

```text
PolkaMeter: verdict INCONCLUSIVE: required checks with no result: no silent loss; exit 1 because of --fail-on-inconclusive (the checks are in summary.md)
```

The line blames a flag only when the flag decides the exit. A smoke-mode run fails on a monitor problem or a required check with no result whatever the flags, and its line says so: `PolkaMeter: smoke mode fails the run on monitor problems and required checks with no result, whatever the --fail-on flags; fix this one: …`. A run that ends `failed` or `stopped` prints its own reason instead. `execution.json` keeps the full line as [`exit_reason`](results.md#executionjson).

With `--remote`, the agent applies `--fail-on-verdict` and `--fail-on-inconclusive` and records the exit code and `exit_reason` in its run directory. The client prints the reason as `PolkaMeter: remote run: <reason>`. If the agent does not support a flag you pass, the client refuses to start the run; update the agent first.

```sh
polkameter run examples/plugin-workflow.polkameter.xml --output runs
```

```text
Phase: preflight
...
Run completed. Artifacts: ~/polkameter/runs/run-1792255021142-12303-1
```

Phase lines go to standard error. The last line, on standard output, names the run directory.

If the run fails, PolkaMeter also prints the reason on standard error, in both formats. The exit code is 1, or 3 when the run failed before its setup phase (see [exit codes](#exit-codes)), and the run directory is kept. Here, a plugin process exited in the middle of a step:

```text
Phase: preflight
Phase: setup
Phase: teardown
Run failed. Artifacts: ~/polkameter/runs/run-1792994214466-72728-1
PolkaMeter: run failed: plugin exited before responding (details in ~/polkameter/runs/run-1792994214466-72728-1/execution.json)
```

The reason stays on one line. Colour and cursor codes are removed, and line breaks show as `⏎`. Other control characters, including the start of other terminal escape sequences such as a window title, are shown escaped, like `\u{1b}`. Hashes of 32 or more hex digits are shortened to their first 12 and `…`. The printed reason is cut at 500 characters with `…`, and `execution.json` keeps the full text. Each message says how to fix the problem before the evidence, so the fix stays on the line.

When the reason points into the plan, the line starts with the plan file, line and column, as `validate` errors do. A run that failed before its setup phase says `run failed before setup:` and exits `3`. It prints no teardown phase, because nothing was submitted:

```text
Phase: preflight
Run failed. Artifacts: ~/polkameter/runs/run-1793948741238-14878-1
PolkaMeter: run failed before setup: plan.polkameter.xml: line 2, column 16: credential `sudo` (profile `sudo`) reads environment variable SUDO_SEED_X (from --credential-env), which is not set; set SUDO_SEED_X on the step that runs polkameter, or pass the variable that holds it to --credential-env (details in ~/polkameter/runs/run-1793948741238-14878-1/execution.json)
```

A failed remote run prints `PolkaMeter: remote run failed: <reason> (details in <run directory>/execution.json on the agent)`. The run directory is on the agent host. See [Troubleshooting](troubleshooting.md#run-failed--details-in-executionjson) for the reasons.

### Stop a run

Press Ctrl-C (SIGINT) or send SIGTERM, as CI systems do when they cancel a job or hit a timeout. PolkaMeter stops the running step and runs the plan's teardown. It then writes `execution.json`, `summary.md` and `junit.xml`, kills the plugin processes and exits with `130` for SIGINT or `143` for SIGTERM. The run's state is `stopped`, even if the signal arrived during teardown.

```text
Phase: preflight
Phase: setup
PolkaMeter: received SIGTERM; stopping the run (teardown has up to 20 s; send the signal again to stop now)
Phase: teardown
Run stopped. Artifacts: ~/polkameter/runs/run-1793933833255-39334-1
```

`execution.json` says whether teardown finished. Its `teardown` field is `completed`, `failed` or `interrupted`:

```json
{
  "artifact_dir": "~/polkameter/runs/run-1793933833255-39334-1",
  "exit_code": 143,
  "state": "stopped",
  "error": "run cancelled",
  "teardown": "completed"
}
```

Teardown can use every plugin, even one whose step was cancelled or that something else killed: PolkaMeter starts that plugin again first. Each teardown step keeps its own `timeout-ms`.

Teardown after a signal is bounded by `--shutdown-grace`, 20 seconds by default. A second signal, or the end of that period, interrupts teardown. PolkaMeter kills the plugins, writes the results and exits with the code of the first signal. `teardown` is then `interrupted`, and `error` ends with `teardown failed: teardown was interrupted by a forced stop`.

```text
PolkaMeter: received SIGTERM; stopping the run (teardown has up to 20 s; send the signal again to stop now)
Phase: teardown
PolkaMeter: stopping now; teardown is interrupted
Run stopped. Artifacts: ~/polkameter/runs/run-1793933833336-39351-1
```

As soon as the first signal arrives, PolkaMeter writes a provisional `execution.json` with the state `stopping`, and redacts credentials from the plugins' `stderr.log` files. If the process is killed before it finishes, that file is what remains:

```json
{
  "artifact_dir": "~/polkameter/runs/run-1793933833336-39351-1",
  "error": "stopped by SIGTERM; teardown was still running",
  "exit_code": 143,
  "state": "stopping",
  "stop_requested_at": 1793933833373
}
```

So exit code `130` or `143` alone does not prove that teardown ran. Check `teardown` in `execution.json`. A state of `stopping` means the run was killed during teardown.

Each plugin runs in its own process group, so a Ctrl-C in the terminal, or a signal that a CI runner sends to PolkaMeter's whole group, reaches only PolkaMeter. When PolkaMeter kills a plugin, it kills the plugin's whole group, including any process the plugin started. On Windows it kills the plugin's process tree.

After a forced stop, PolkaMeter waits at most 10 seconds for the run to write its outcome, then exits anyway. A stop can therefore take up to `--shutdown-grace` plus 10 seconds. Set `--shutdown-grace` to the time your CI system allows between SIGTERM and SIGKILL, minus 10 seconds:

| Where | Time from SIGTERM to SIGKILL | `--shutdown-grace` |
| --- | --- | --- |
| Kubernetes (`terminationGracePeriodSeconds`, default 30 s) | 30 s | `20` (the default) |
| GitLab Runner | Depends on the executor; on Kubernetes, the pod's grace period | That time minus 10 |
| `docker stop` (default `-t 10`) | 10 s | `0`, or raise `docker stop -t` / `--stop-timeout` |
| GitHub Actions cancellation | SIGINT, then SIGTERM 7.5 s later, then SIGKILL 2.5 s after that | The SIGTERM is the second signal and interrupts teardown |

> **Warning:** SIGKILL cannot be handled. If it comes first, the plugins are not killed and the results are not written. In a container, run PolkaMeter under an init process (`docker run --init`, or `tini`), so that signals reach it and processes it leaves behind are reaped.

With `--remote`, the first signal asks the agent to stop the run, and the client waits for the outcome. A second signal, or the end of `--shutdown-grace`, asks the agent to interrupt teardown at once, even if the first stop request is still waiting for an answer. If the agent has not reported the outcome 10 seconds after that, the client stops waiting and exits with the signal's code; the run directory stays on the agent.

```text
PolkaMeter: received SIGINT; asking the agent to stop the run (teardown has up to 20 s; send the signal again to stop now)
Remote run stopped. Artifacts: /data/runs/run-1793933833445-39378-2
```

## report

Rebuilds a run's outputs from its files without running the plan.

```sh
polkameter report [--junit PATH] [--format human|json] ARTIFACT_DIRECTORY
```

| Name | Type | Required | Default | Limits | Meaning |
| --- | --- | --- | --- | --- | --- |
| `ARTIFACT_DIRECTORY` | path | yes | none | holds `execution.json` and `summary.md` | The run directory |
| `--junit PATH` | path | no | none | <!-- v0.3.0: remove after release -->from v0.3.0 | An additional copy of `junit.xml` |
| `--format` | enum | no | `human` | `human` or `json` | See [output formats](#output-formats) |

With a `<load>`, it rebuilds the built-in and plugin checks, `summary.json`, `summary.md`, `junit.xml`, `samples.jtl` and `plots/`. Without one, it rewrites `junit.xml`, `samples.jtl` and `plots/`. The directory must hold `execution.json` and `summary.md`. The human format prints the summary.

`report` exits 0 when the rebuild succeeds, even if the outcome is `failed`. Check the `Status:` line or `execution.json`'s `state`.

If `summary.json` has a `schemaVersion` other than `1`, or none, `report` changes no file and exits 2. See [rebuilding a report](results.md#rebuilding-a-report).

```sh
polkameter report runs/run-1792255021142-12303-1
```

```text
# Plugin data flow

Status: completed
...
```

## plugin

Installs plugins and sets this host's credential profiles, topologies and capabilities. A plan names these; the host holds the values. `install` prints one line. The other subcommands print pretty JSON. None of them take `--format`.

The registry is a JSON file; [Environment variables](#environment-variables) sets its path. See [Plugins](plugins.md) and [writing a plugin](writing-a-plugin.md).

You can run several `plugin` commands at once, for example to install plugins in parallel CI steps. Each command that changes the registry locks `<registry>.lock` next to it, rereads the registry and replaces it in one step, so no change is lost. A command that has to wait for the lock prints `PolkaMeter: waiting for <registry>.lock (another polkameter is updating the registry)` first. If the registry path is a symbolic link, PolkaMeter writes the file it points to and keeps the link. A new registry is readable only by you; a replaced one keeps its permissions. A registry that is not valid JSON fails every `plugin` command with exit code 2 and is left as it is; see [Troubleshooting](troubleshooting.md#plugin-registry-path-is-not-a-valid-registry-).

Every `plugin` subcommand exits `2` when it fails, including an install whose executable cannot be started or does not answer `$describe`.

### plugin install

Records the plugin's ID, version and BLAKE2 hash. Install it again after each rebuild: PolkaMeter refuses to run a plugin whose file has changed.

```sh
polkameter plugin install EXECUTABLE
```

| Name | Type | Required | Default | Limits | Meaning |
| --- | --- | --- | --- | --- | --- |
| `EXECUTABLE` | path | yes | none | a plugin that answers `$describe` | The plugin executable |

```sh
polkameter plugin install target/debug/polkameter-example-plugin
```

```text
Installed plugin example 0.1.0 (6 operations) from ~/polkameter/target/debug/polkameter-example-plugin
```

### plugin list

Lists what this host's registry holds: the installed plugins, the environment variable each credential profile reads, the topologies and the capabilities. It shows variable names, never their values.

```sh
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

### plugin inspect

Starts the plan's plugins and checks each operation against its manifest. It prints the manifests, evidence, targets, monitor requirements and metric catalog, and submits nothing.

```sh
polkameter plugin inspect PLAN
```

| Name | Type | Required | Default | Limits | Meaning |
| --- | --- | --- | --- | --- | --- |
| `PLAN` | path | yes | none | a file or pipe of at most 2 MiB | The plan file |

```sh
polkameter plugin inspect examples/plugin-workflow.polkameter.xml
```

```json
{
  "metricCatalog": [
    ...
```

### plugin credential

Maps a plan's credential profile to an environment variable on this host. The registry stores the variable's name, not its value. PolkaMeter reads the variable at the start of a run, and `preflight` checks every credential the plan declares. An unmapped profile, or an unset or empty variable, fails both with exit code `3` and a message that names the variable; see [Troubleshooting](troubleshooting.md#credentials-and-topologies).

```sh
polkameter plugin credential PROFILE ENV_VAR
```

| Name | Type | Required | Default | Limits | Meaning |
| --- | --- | --- | --- | --- | --- |
| `PROFILE` | string | yes | none | none | The profile the plan's `<credential>` names |
| `ENV_VAR` | variable name | yes | none | set and not empty when a run starts | The environment variable that holds the secret |

```sh
polkameter plugin credential sudo SUDO_SEED
```

```json
{
  "configured": true
}
```

### plugin topology

Registers a Zombienet `zombie.json` under the alias that `<monitors topology="…">` names. The path is stored as an absolute path. A path that does not exist fails with exit code 2.

```sh
polkameter plugin topology NAME PATH
```

| Name | Type | Required | Default | Limits | Meaning |
| --- | --- | --- | --- | --- | --- |
| `NAME` | string | yes | none | none | The alias that the plan's `<monitors>` uses |
| `PATH` | path | yes | none | must exist | The topology file, stored as an absolute path |

```sh
polkameter plugin topology zombie ./zombie.json
```

```json
{
  "configured": true
}
```

### plugin capability

Records a capability the network provisioner supplies. Preflight checks that a plugin's required capabilities are recorded.

```sh
polkameter plugin capability NAME
```

| Name | Type | Required | Default | Limits | Meaning |
| --- | --- | --- | --- | --- | --- |
| `NAME` | string | yes | none | none | The capability name, as the plugin requires it |

```sh
polkameter plugin capability zombienet
```

```json
{
  "configured": true
}
```

## agent serve

Starts the remote agent on this host. It runs plans that clients send with `polkameter run --remote`, using its own plugins, credentials and topologies.

```sh
polkameter agent serve [--bind ADDR] [--token-env ENV_VAR] [--output-root DIR] [--keep-runs N] [--shutdown-grace SECONDS]
```

| Name | Type | Required | Default | Limits | Meaning |
| --- | --- | --- | --- | --- | --- |
| `--bind ADDR` | address | no | `127.0.0.1:9901` | a loopback address | The address to listen on |
| `--token-env` | variable name | no | `POLKAMETER_AGENT_TOKEN` | set; at least 32 characters with at least 8 different characters | The variable that holds the bearer token; generate it with `openssl rand -hex 32`. There is no flag for the token itself |
| `--output-root DIR` | path | no | `target/polkameter-agent-runs` | none | Where run directories go. Relative paths start from the agent's working directory |
| `--keep-runs N` | number | no | keep all | at least 1 | Keep the newest `N` finished run directories. When a run starts, older finished `run-…` directories under `--output-root` are deleted. A run is finished when its `execution.json` holds a final state and no process holds its `run.lock`. Runs still in progress, runs killed while stopping, and other files are never touched |
| `--shutdown-grace SECONDS` | seconds | no | `20` | none | How long the active run gets to finish teardown after SIGINT or SIGTERM |

The agent does not serve TLS; remote clients need an SSH tunnel or TLS proxy.

On SIGINT or SIGTERM, the agent refuses new runs and stops the active run, which still runs its teardown within `--shutdown-grace`. It keeps answering status requests until the run's client has read the outcome, for at most 5 seconds. Then it exits with `130` for SIGINT or `143` for SIGTERM. A second signal interrupts teardown. A run that is still being admitted (its plan inspected) when the signal arrives is not started.

Give each agent its own `--output-root`. With `--keep-runs`, an agent never deletes a run another process is still writing, but sharing a root makes the count of kept runs cover both.

```text
PolkaMeter: received SIGTERM; stopping the active run (teardown has up to 20 s; send the signal again to stop now)
```

Check the agent. `/health` needs no token:

```sh
POLKAMETER_AGENT_TOKEN=$(openssl rand -hex 32) polkameter agent serve --bind 127.0.0.1:9901 &
curl http://127.0.0.1:9901/health
```

```text
{"status":"ok"}
```

### Remote agent

Every route except `/health` needs `Authorization: Bearer TOKEN`. A missing or wrong token gets `401 Unauthorized`. Requests and responses are JSON; a plan is sent as `{"xml": "..."}`.

`POST /runs` also accepts `"options": {"failOnVerdict": true, "failOnInconclusive": true}`; each defaults to `false`. `GET /plugins` advertises support for each as `"runOptions": {"failOnVerdict": true, "failOnInconclusive": true}`. An agent ignores options it does not know, so a client checks this first.

`POST /runs` also accepts a `"runId"` chosen by the client, with the same character rules as other run IDs. Sending the same `runId` again returns that run instead of starting another, so a client whose start request lost its answer can repeat it. While the first request is still being admitted, a repeat gets `503` and should be retried. `polkameter run --remote` always sends one.

| Method and path | Purpose |
| --- | --- |
| `GET /health` | Liveness |
| `GET /plugins` | Protocol version and installed plugins |
| `POST /inspect`, `POST /preflight` | Same checks as `plugin inspect` and `preflight` |
| `POST /runs` | Starts a run, or returns the run already started with the same `runId`. Returns its status and run ID |
| `GET /runs/{id}` | State, phase, and the outcome once it ends |
| `POST /runs/{id}/stop` | Stops a running run; teardown still runs. Add `?force=true` to interrupt teardown too |

The client checks each run ID: at most 128 characters, from letters, digits, `-`, `_` and `.`.

- **Transport.** The client accepts `https://`, or `http://` only when the host is a loopback address (`127.0.0.0/8`, `[::1]` or `localhost`). It refuses an endpoint with user information (`user@` or `user:password@`), a query or a fragment. It never follows redirects, so the token goes only to the endpoint you name.
- **Timeouts.** Connecting may take 10 seconds. A request may take 60 seconds, or 10 minutes for `/inspect` and `/preflight`, and a stop request 5 seconds. A failed start request or status poll is retried with backoff, from 0.6 up to 5 seconds apart, for 60 seconds; a start is repeated with the same `runId`. The client's deadline is the plan's `timeout-ms` plus its teardown step timeouts and one minute, counted from before the start request; no start request, status poll or wait goes past it. If retries run out or the deadline passes, the client asks the agent to stop the run (one stop request of at most 5 seconds), then exits `1`.
- **Trust.** The bearer token grants full execution trust. A plan holding it can use every plugin, credential profile and topology on the agent, and name any endpoint in its `<targets>`. Installed plugins are not sandboxed, so use a separate agent and token for each trust domain.
- **Wire.** Only plan XML is sent, never executable paths or secrets.

[Run on a remote machine](remote-runs.md) covers setup and what the agent refuses.

## Exit codes

| Code | Meaning | Typical cause |
| --- | --- | --- |
| `0` | A command succeeded, or a run ended `completed` | Nothing to fix |
| `1` | A run ended `failed` after its setup phase started, or `completed_with_failures`, or a `report` or remote request failed, or the output could not be written | A plugin crashed or errored in setup or later, a deadline or assertion failed, or the agent refused a request. A smoke-mode load run also exits 1 on a monitor problem or a missing required check. With `--fail-on-verdict`, a `FAIL` verdict also exits 1, and with `--fail-on-inconclusive`, an `INCONCLUSIVE` verdict. A command that would exit 0 exits 1 when standard output fails, for example on a full disk |
| `2` | Invalid input or usage | A malformed or invalid plan, a plan file over 2 MiB or that is a directory, socket or device, a bad flag or argument, an `--output` path that is not UTF-8, an unreadable or corrupt registry, a failed `plugin` subcommand, a wrong remote endpoint or token variable, or a `summary.json` that `report` cannot rewrite (unknown `schemaVersion`) |
| `3` | Preflight or setup checks failed | `preflight` failed, or a `run` failed before its setup phase, so it submitted nothing: a credential that is not mapped, unset or empty, a plugin not installed, a checksum, version or manifest mismatch, an operation or input that does not match its manifest, a preflight step that can change state or fails, a chain that lacks required evidence, no calibration answer from the chain, or an unregistered `<monitors>` alias |
| `130` | Stopped by SIGINT | Ctrl-C during a run, or a remote run that the agent stopped on request. `agent serve` also exits `130` after SIGINT |
| `143` | Stopped by SIGTERM | A CI system cancelled the job or hit its timeout, or the agent of a remote run received SIGTERM. `agent serve` also exits `143` after SIGTERM |

A stopped run usually ran its teardown and wrote its results, but not always: a second signal interrupts teardown, and a forced stop that cannot write its outcome within 10 seconds exits anyway. Check `execution.json`: `teardown` is `completed`, `failed` or `interrupted`, and a `state` of `stopping` means the outcome was never written. See [stop a run](#stop-a-run).

A `run` that exits `3` failed before its setup phase. It submitted nothing to the chain and skipped teardown, because there was nothing to undo. Its `execution.json` has `state` `failed`, `exit_code` `3` and `failed_phase` `preflight`. Run `polkameter preflight <plan>` before `run` to find these problems without creating a run directory.

By default, a stress-mode load run exits `0` whatever its verdict. Add `--fail-on-verdict` to exit `1` on `FAIL`, and `--fail-on-inconclusive` to exit `1` on `INCONCLUSIVE`. Both use the existing failed-run code; `WARN` never triggers them. The [GitHub Action](ci.md#what-fails-the-job) passes both by default. See [reading the results](reading-results.md).

## Environment variables

| Variable | Read by | Effect |
| --- | --- | --- |
| `POLKAMETER_PLUGIN_REGISTRY` | Commands that use installed plugins | Path of the plugin registry (JSON). Default: `~/.config/polkameter/plugins.json` |
| `HOME`, `USERPROFILE` | The default registry path | Home directory for the default path. `USERPROFILE` is used if `HOME` is unset. If neither is set, set `POLKAMETER_PLUGIN_REGISTRY` |
| `POLKAMETER_NODE_PID` | Load runs | The node's process ID, for CPU and memory. If unset and the target URL is loopback, PolkaMeter finds the process on the RPC port with `lsof`, then `ss`, skipping port forwarders such as `ssh`. If none is found, no CPU or memory is recorded. See [Node CPU and memory](how-it-works.md#node-cpu-and-memory) |
| `POLKAMETER_AGENT_TOKEN` | `agent serve` | Default name of the token variable, used when `--token-env` is not given. At least 32 characters, at least 8 of them different |

Plugins start with a cleared environment. Only `PATH`, `TMPDIR`, `TEMP`, `SYSTEMROOT`, `SSL_CERT_FILE` and `SSL_CERT_DIR` are passed on. A plugin receives credential values only as step inputs.

## Output formats

With `--format json`, `validate`, `preflight`, `run` and `report` print one JSON object per line on standard output, including when they fail.

```sh
polkameter validate examples/plugin-workflow.polkameter.xml --format json
```

```json
{"event":"validation","plan":"Plugin data flow","schemaVersion":1,"valid":true}
```

A plan with an error prints the error on standard error, as in human mode, and an `error` line on standard output. The command exits with the same code:

```sh
polkameter validate ref.xml --format json
```

```text
PolkaMeter: ref.xml: line 3, column 34: step `a` (core.echo): input `value` refers to `steps.nope.value`, but nothing in the plan defines it; use targets.<id>, credentials.<id>, run.id, run.directory, run.probeBudget, steps.<earlier step>.<output>, or in a workflow user.index or iteration.index
```

```json
{"column":34,"command":"validate","event":"error","exitCode":2,"file":"ref.xml","kind":"invalid","line":3,"message":"ref.xml: line 3, column 34: step `a` (core.echo): input `value` refers to `steps.nope.value`, but nothing in the plan defines it; use targets.<id>, credentials.<id>, run.id, run.directory, run.probeBudget, steps.<earlier step>.<output>, or in a workflow user.index or iteration.index","schemaVersion":1,"valid":false}
```

### JSON output contract

Every line is one JSON object with these fields:

| Field | Type | Meaning |
| --- | --- | --- |
| `event` | string | What the line reports. See the table below |
| `schemaVersion` | integer | The version of this output, `1`. It changes only when a field is removed or renamed, or its meaning changes. New fields and events can appear without a new version |

| `event` | Printed by | Other fields |
| --- | --- | --- |
| `validation` | `validate`, last line | `valid` (`true`), `plan` (the plan's `name`) |
| `preflight` | `preflight`, last line | `valid`, `plugins` (each plugin's manifest), `requirements`, `targets`, `monitorRequirements`, `metricCatalog`, and `calibration` for a plan with a load |
| `phase`, `step-started`, `step-finished`, `step-progress`, `warning`, `completed`, … | a local `run` | The events of [events.jsonl](results.md#eventsjsonl), as the run writes them |
| `artifact-written` | `run`, last line | `outcome`: the content of the run's `execution.json`. When a failed run's error points into the plan, also `file`, `line` and `column`, as in an `error` line |
| `report` | `report`, last line | `outcome` (as above) and `summary` (the text of `summary.md`) |
| `error` | any of the four commands, when it fails before it has a result | `command`, `kind` (`invalid`, `preflight` or `runtime`), `exitCode`, `message` (the text printed after `PolkaMeter:` on standard error). For a plan error, also `file`, and `line` and `column` when the error has a position. A `file` that is not UTF-8 has its invalid bytes replaced with `\ufffd`. `validate` also sets `valid` to `false` |

Keys that PolkaMeter adds to a line are camelCase, as in `summary.json`. Two kinds of nested objects keep the keys of the format they copy, and so may be snake_case:

- `outcome` is `execution.json` (`artifact_dir`, `exit_code`, `failed_phase`, `error_location`, `exit_reason`, `stop_requested_at`), which [Results](results.md) documents.
- The plugin manifests in `preflight` use the plugin protocol's keys, such as `read_only`.

Other rules:

- **Standard error** keeps the human messages in both formats: errors as `PolkaMeter: <message>`, and a failed run's `PolkaMeter: run failed: <reason> (details in …)`, or `run failed before setup:` for exit code 3. The reason line is cut at 500 characters, with hashes shortened to 12; each message says how to fix it before its evidence. A failed run's full reason is the `error` field of the `completed` and `artifact-written` outcomes.
- **Load runs** also print `stop:`, `loss check:` and `smoke:` lines, and the summary, to standard error.
- **Remote runs** print only `artifact-written`, because the agent does not stream events.
- **Argument errors**, such as an unknown flag, are printed by the argument parser as text and exit `2` before any JSON is written.
- **A closed standard output**, as with `| head -1`, stops the output without an error. The command carries on: a run still finishes, writes its run directory and exits with its own code. A closed standard error is ignored the same way.
- **Other output errors**, such as a full disk, stop the output and print `PolkaMeter: cannot write to standard output: <error>` once on standard error. The command carries on, and a command that would exit `0` exits `1`.

The `plugin` subcommands do not take `--format`. `plugin install` prints one line, and the others print pretty JSON.
