# CLI reference

`polkameter` validates, preflights and runs plans, reports on runs, manages plugins and serves a remote agent. Run `polkameter COMMAND --help` for one command's flags. Errors: see [Troubleshooting](troubleshooting.md).

## --version

Prints `polkameter <version>` and exits 0. `<version>` is the version of the build you installed. `-V` is the short form.

```sh
polkameter --version
```

```text
polkameter <version>
```

## validate

Checks a plan's structure without starting plugins or connecting to a chain. [Plans](plans.md#validation-rules) lists the rules.

```sh
polkameter validate [--format human|json] PLAN
```

| Name | Default | Meaning |
| --- | --- | --- |
| `PLAN` | none | The plan file |
| `--format` | `human` | `human` or `json`. See [output formats](#output-formats) |

From the repository root:

```sh
polkameter validate examples/plugin-workflow.polkameter.xml
```

```text
Plan is structurally valid. Preflight checks the installed plugins.
```

## preflight

Checks each operation against its plugin's manifest and the evidence the plugins require. It also checks the chain and runs the read-only preflight steps, submitting nothing. A plan with a load also measures the block interval. Install plugins first: [`plugin install`](#plugin-install).

```sh
polkameter preflight [--credential-env ENV_VAR] [--format human|json] PLAN
```

| Name | Default | Meaning |
| --- | --- | --- |
| `PLAN` | none | The plan file |
| `--credential-env` | none | Reads the only credential from this variable. The plan must declare exactly one |
| `--format` | `human` | `human` or `json` |

```sh
polkameter preflight examples/plugin-workflow.polkameter.xml
```

```text
Preflight passed.
```

## run

Runs a plan. A local run writes a run directory under `--output`. A remote run sends the plan's XML to an agent, which runs it and keeps the run directory there.

```sh
polkameter run [--output DIR] [--junit PATH] [--credential-env ENV_VAR] [--fail-on-verdict] [--shutdown-grace SECONDS] [--format human|json] PLAN
polkameter run --remote URL --remote-token-env ENV_VAR [--junit PATH] [--fail-on-verdict] [--shutdown-grace SECONDS] [--format human|json] PLAN
```

| Name | Required | Meaning |
| --- | --- | --- |
| `PLAN` | yes | The plan file |
| `--output DIR` | yes, unless `--remote` | The artifact root |
| `--junit PATH` | no | Write a local copy of the run directory's `junit.xml`, including for a remote run. Default: no additional copy |
| `--credential-env` | no | Reads the only credential from this variable. Not allowed with `--remote` |
| `--fail-on-verdict` | no | Default off. Exit `1` on a `FAIL` load verdict after writing the report and running teardown. Other verdicts and plans without a load test keep their existing exit behavior |
| `--remote URL` | no | The agent's endpoint. See [remote agent](#remote-agent) |
| `--remote-token-env` | with `--remote` | The variable that holds the agent's bearer token |
| `--shutdown-grace SECONDS` | no | Default `30`. How long a stopping run gets to finish teardown after SIGINT or SIGTERM. See [stop a run](#stop-a-run) |
| `--format` | no | `human` (default) or `json`. See [output formats](#output-formats) |

With `--remote`, the agent applies `--fail-on-verdict` and records the exit code in its run directory. If the agent does not support the flag, the client refuses to start the run; update the agent first.

```sh
polkameter run examples/plugin-workflow.polkameter.xml --output runs
```

```text
Phase: preflight
...
Run completed. Artifacts: ~/polkameter/runs/run-1792255021142-12303-1
```

Phase lines go to standard error. The last line, on standard output, names the run directory.

If the run fails, PolkaMeter also prints the reason on standard error, in both formats. The exit code is 1 and the run directory is kept. Here, a plugin process exited in the middle of a step:

```text
Phase: preflight
Phase: setup
Phase: teardown
Run failed. Artifacts: ~/polkameter/runs/run-1792994214466-72728-1
Polkameter: run failed: plugin exited before responding (details in ~/polkameter/runs/run-1792994214466-72728-1/execution.json)
```

The reason stays on one line. Colour and cursor codes are removed, and line breaks show as `⏎`. Other control characters, including the start of other terminal escape sequences such as a window title, are shown escaped, like `\u{1b}`. The printed reason is cut at 300 characters with `…`, and `execution.json` keeps the full text.

A failed remote run prints `Polkameter: remote run failed: <reason> (details in <run directory>/execution.json on the agent)`. The run directory is on the agent host. See [Troubleshooting](troubleshooting.md#run-failed--details-in-executionjson) for the reasons.

### Stop a run

Press Ctrl-C (SIGINT) or send SIGTERM, as CI systems do when they cancel a job or hit a timeout. PolkaMeter stops the running step and runs the plan's teardown. It then writes `execution.json`, `summary.md` and `junit.xml`, kills and reaps the plugin processes, and exits with `130` for SIGINT or `143` for SIGTERM. The run's state is `stopped`.

```text
Phase: preflight
Phase: setup
Polkameter: received SIGTERM; stopping the run (teardown has up to 30 s; send the signal again to stop now)
Phase: teardown
Run stopped. Artifacts: ~/polkameter/runs/run-1793901167685-75157-1
```

Teardown can use every plugin, even one whose step was cancelled: PolkaMeter starts that plugin again first. Each teardown step keeps its own `timeout-ms`.

Teardown after a signal is bounded by `--shutdown-grace`, 30 seconds by default. A second signal, or the end of that period, interrupts teardown: PolkaMeter kills the plugins, writes the results and exits with the code of the first signal. The run's `error` then ends with `teardown failed: teardown was interrupted by a forced stop`.

```text
Polkameter: received SIGTERM; stopping the run (teardown has up to 30 s; send the signal again to stop now)
Phase: teardown
Polkameter: stopping now; teardown is interrupted
Run stopped. Artifacts: ~/polkameter/runs/run-1793901169005-75228-1
```

> **Warning:** Give the job more time than `--shutdown-grace` between SIGTERM and SIGKILL. SIGKILL cannot be handled: the plugins keep running and the results are not written. GitHub Actions, for example, sends SIGINT, then SIGTERM 7.5 seconds later, then SIGKILL 2.5 seconds after that, so its second signal interrupts teardown.

With `--remote`, the first signal asks the agent to stop the run, and the client waits for the outcome. A second signal, or the end of `--shutdown-grace`, asks the agent to interrupt teardown. If the agent still has not reported the outcome 10 seconds later, the client stops waiting and exits; the run directory stays on the agent.

```text
Polkameter: received SIGINT; asking the agent to stop the run (teardown has up to 30 s; send the signal again to stop now)
Remote run stopped. Artifacts: /data/runs/run-1793901168078-75180-2
```

## report

Rebuilds a run's outputs from its files without running the plan.

```sh
polkameter report [--junit PATH] [--format human|json] ARTIFACT_DIRECTORY
```

| Name | Default | Meaning |
| --- | --- | --- |
| `ARTIFACT_DIRECTORY` | none | The run directory |
| `--junit PATH` | none | Write an additional copy of `junit.xml` |
| `--format` | `human` | `human` or `json`. See [output formats](#output-formats) |

With a `<load>`, it rebuilds the built-in and plugin checks, `summary.json`, `summary.md`, `junit.xml`, `samples.jtl` and `plots/`. Without one, it rewrites `junit.xml`, `samples.jtl` and `plots/`. The directory must hold `execution.json` and `summary.md`. The human format prints the summary.

`report` exits 0 when the rebuild succeeds, even if the outcome is `failed`. Check the `Status:` line or `execution.json`'s `state`.

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

### plugin install

Records the plugin's ID, version and BLAKE2 hash. Install it again after each rebuild: PolkaMeter refuses to run a plugin whose file has changed.

```sh
polkameter plugin install EXECUTABLE
```

| Argument | Meaning |
| --- | --- |
| `EXECUTABLE` | The plugin executable |

```sh
polkameter plugin install target/debug/polkameter-example-plugin
```

```text
Installed plugin example 0.1.0 (6 operations) from ~/polkameter/target/debug/polkameter-example-plugin
```

### plugin list

Lists the installed plugins.

```sh
polkameter plugin list
```

```json
{
  "example": {
    "blake2": "2037888f190d19169cab991cd38c690a88a2c11a15bc982b1eb796fbe6c7d493",
    ...
```

### plugin inspect

Starts the plan's plugins and checks each operation against its manifest. It prints the manifests, evidence, targets, monitor requirements and metric catalog, and submits nothing.

```sh
polkameter plugin inspect PLAN
```

| Argument | Meaning |
| --- | --- |
| `PLAN` | The plan file. The help text calls it `SCENARIO` |

```sh
polkameter plugin inspect examples/plugin-workflow.polkameter.xml
```

```json
{
  "metricCatalog": [
    ...
```

### plugin credential

Maps a plan's credential profile to an environment variable on this host. The registry stores the variable's name, not its value. PolkaMeter reads the variable at the start of a run. An unset or empty variable fails the run.

```sh
polkameter plugin credential <PROFILE> <ENV>
```

| Argument | Meaning |
| --- | --- |
| `PROFILE` | The profile the plan's `<credential>` names |
| `ENV_VAR` | The environment variable that holds the secret |

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

| Argument | Meaning |
| --- | --- |
| `NAME` | The alias that the plan's `<monitors>` uses |
| `PATH` | The topology file |

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

| Argument | Meaning |
| --- | --- |
| `NAME` | The capability name, as the plugin requires it |

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

| Flag | Default | Meaning |
| --- | --- | --- |
| `--bind ADDR` | `127.0.0.1:9901` | The address to listen on, which must be loopback |
| `--token-env` | `POLKAMETER_AGENT_TOKEN` | The variable that holds the bearer token. It must be set and at least 32 characters long. There is no flag for the token itself |
| `--output-root DIR` | `target/polkameter-agent-runs` | Where run directories go. Relative paths start from the agent's working directory |
| `--keep-runs N` | keep all | Keep only the newest `N` run directories (at least 1). Older `run-…` directories under `--output-root` are deleted when a run starts. Other files are never touched |
| `--shutdown-grace SECONDS` | `30` | How long the active run gets to finish teardown after SIGINT or SIGTERM |

The agent does not serve TLS; remote clients need an SSH tunnel or TLS proxy.

On SIGINT or SIGTERM, the agent refuses new runs and stops the active run, which still runs its teardown within `--shutdown-grace`. It keeps answering status requests until the run's client has read the outcome, for at most 5 seconds. Then it exits with `130` for SIGINT or `143` for SIGTERM. A second signal interrupts teardown.

```text
Polkameter: received SIGTERM; stopping the active run (teardown has up to 30 s; send the signal again to stop now)
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

`POST /runs` also accepts `"options": {"failOnVerdict": true}`; the default is `false`. `GET /plugins` advertises support as `"runOptions": {"failOnVerdict": true}`.

| Method and path | Purpose |
| --- | --- |
| `GET /health` | Liveness |
| `GET /plugins` | Protocol version and installed plugins |
| `POST /inspect`, `POST /preflight` | Same checks as `plugin inspect` and `preflight` |
| `POST /runs` | Starts a run. Returns its status and run ID |
| `GET /runs/{id}` | State, phase, and the outcome once it ends |
| `POST /runs/{id}/stop` | Stops a running run; teardown still runs. Add `?force=true` to interrupt teardown too |

The client checks each run ID: at most 128 characters, from letters, digits, `-`, `_` and `.`.

- **Transport.** The client accepts `https://`, or `http://` only when the host is a loopback address (`127.0.0.0/8`, `[::1]` or `localhost`). It refuses an endpoint with user information (`user@` or `user:password@`), a query or a fragment. It never follows redirects, so the token goes only to the endpoint you name.
- **Timeouts.** Connecting may take 10 seconds. A request may take 60 seconds, or 10 minutes for `/inspect` and `/preflight`. A failed status poll is retried with backoff, from 0.6 up to 5 seconds apart, for 60 seconds. If polls keep failing, or the run lasts longer than the plan's `timeout-ms` plus its teardown step timeouts and one minute, the client asks the agent to stop the run, then exits `1`.
- **Trust.** The bearer token grants full execution trust. A plan holding it can use every plugin, credential profile and topology on the agent, and name any endpoint in its `<targets>`. Installed plugins are not sandboxed, so use a separate agent and token for each trust domain.
- **Wire.** Only plan XML is sent, never executable paths or secrets.

[Run on a remote machine](remote-runs.md) covers setup and what the agent refuses.

## Exit codes

| Code | Meaning | Typical cause |
| --- | --- | --- |
| `0` | A command succeeded, or a run ended `completed` | Nothing to fix |
| `1` | A run ended `failed` or `completed_with_failures`, or a `report` or remote request failed | A plugin crashed or errored, a deadline or assertion failed, a credential was unavailable, or the agent refused a request. A smoke-mode load run also exits 1 on a monitor problem or a missing required check. With `--fail-on-verdict`, a `FAIL` verdict also exits 1 |
| `2` | Invalid input or usage | A malformed or invalid plan, a bad flag or argument, an unreadable registry, or a wrong remote endpoint or token variable |
| `3` | Preflight failed | A plugin not installed, a checksum or version mismatch, an operation missing from its manifest, a preflight step that can change state, no calibration answer from the chain, or an unregistered `<monitors>` alias |
| `130` | Stopped by SIGINT | Ctrl-C during a run, or a remote run that the agent stopped on request. Teardown ran and the results were written. `agent serve` also exits `130` after SIGINT |
| `143` | Stopped by SIGTERM | A CI system cancelled the job or hit its timeout, or the agent of a remote run received SIGTERM. Teardown ran and the results were written. `agent serve` also exits `143` after SIGTERM |

A `run` never exits with `3`. A preflight failure inside a run makes it `failed`, with exit code 1.

By default, a stress-mode load run exits `0` whatever its verdict. Add `--fail-on-verdict` to exit `1` on `FAIL`. This uses the existing failed-run code; `WARN` and `INCONCLUSIVE` do not trigger it. See [reading the results](reading-results.md).

## Environment variables

| Variable | Read by | Effect |
| --- | --- | --- |
| `POLKAMETER_PLUGIN_REGISTRY` | Commands that use installed plugins | Path of the plugin registry (JSON). Default: `~/.config/polkameter/plugins.json` |
| `HOME`, `USERPROFILE` | The default registry path | Home directory for the default path. `USERPROFILE` is used if `HOME` is unset. If neither is set, set `POLKAMETER_PLUGIN_REGISTRY` |
| `POLKAMETER_NODE_PID` | Load runs with `<monitors>` | The node's process ID, for CPU and memory. If unset, PolkaMeter finds the process on the RPC port with `lsof`, then `ss`. If none is found, no CPU or memory is recorded |
| `POLKAMETER_AGENT_TOKEN` | `agent serve` | Default name of the token variable, used when `--token-env` is not given. At least 32 characters |

Plugins start with a cleared environment. Only `PATH`, `TMPDIR`, `TEMP`, `SYSTEMROOT`, `SSL_CERT_FILE` and `SSL_CERT_DIR` are passed on. A plugin receives credential values only as step inputs.

## Output formats

With `--format json`, `validate`, `preflight`, `run` and `report` print one JSON object per line on standard output.

```sh
polkameter validate examples/plugin-workflow.polkameter.xml --format json
```

```json
{"event":"validation","plan":"Plugin data flow","valid":true}
```

- **Errors** go to standard error as `Polkameter: <message>`, even with `--format json`. A failed run adds `Polkameter: run failed: <reason> (details in …)` there.
- **Standard output** keeps only JSON lines. A failed run's full reason is the `error` field of the `completed` and `artifact-written` outcomes.
- **Load runs** also print `stop:`, `loss check:` and `smoke:` lines, and the summary, to standard error.

The CLI adds a final line: `artifact-written` for `run`, `validation` for `validate`, `report` for `report`, with the summary text. A remote run prints only `artifact-written`, because the agent does not stream events. Fields are in [events.jsonl](results.md#eventsjsonl).
