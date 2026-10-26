# CLI reference

`polkameter` validates, preflights and runs plans, reports on runs, manages plugins and serves a remote agent. Run `polkameter COMMAND --help` for one command's flags. Errors: see [Troubleshooting](troubleshooting.md).

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
polkameter run [--output DIR] [--credential-env ENV_VAR] [--fail-on-verdict] [--format human|json] PLAN
polkameter run --remote URL --remote-token-env ENV_VAR [--fail-on-verdict] [--format human|json] PLAN
```

| Name | Required | Meaning |
| --- | --- | --- |
| `PLAN` | yes | The plan file |
| `--output DIR` | yes, unless `--remote` | The artifact root |
| `--credential-env` | no | Reads the only credential from this variable. Not allowed with `--remote` |
| `--fail-on-verdict` | no | Default off. Exit `1` on a `FAIL` load verdict after writing the report and running teardown. Other verdicts and plans without a load test keep their existing exit behavior |
| `--remote URL` | no | The agent's endpoint. See [remote agent](#remote-agent) |
| `--remote-token-env` | with `--remote` | The variable that holds the agent's bearer token |
| `--format` | no | `human` (default) or `json`. See [output formats](#output-formats) |

With `--remote`, the agent applies `--fail-on-verdict` and records the exit code in its run directory. If the agent does not support the flag, the client refuses to start the run; update the agent first.

Ctrl-C stops a local run, keeping what it wrote. With `--remote`, Ctrl-C asks the agent to stop the run and waits for the outcome.

```sh
polkameter run examples/plugin-workflow.polkameter.xml --output runs
```

```text
Phase: preflight
...
Run completed. Artifacts: ~/polkameter/runs/run-1792255021142-12303-1
```

Phase lines go to standard error. The last line, on standard output, names the run directory.

## report

Rebuilds a run's outputs from its files without running the plan.

```sh
polkameter report [--format human|json] ARTIFACT_DIRECTORY
```

| Name | Default | Meaning |
| --- | --- | --- |
| `ARTIFACT_DIRECTORY` | none | The run directory |
| `--format` | `human` | `human` or `json`. See [output formats](#output-formats) |

With a `<load>`, it rebuilds the built-in and plugin checks, `summary.json`, `summary.md`, `samples.jtl` and `plots/`. Without one, it rewrites `samples.jtl` and `plots/`. The directory must hold `execution.json` and `summary.md`. The human format prints the summary.

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
polkameter agent serve [--bind ADDR] [--token-env ENV_VAR] [--output-root DIR]
```

| Flag | Default | Meaning |
| --- | --- | --- |
| `--bind ADDR` | `127.0.0.1:9901` | The address to listen on, which must be loopback |
| `--token-env` | `POLKAMETER_AGENT_TOKEN` | The variable that holds the bearer token, which must be set and not empty. There is no flag for the token itself |
| `--output-root DIR` | `target/polkameter-agent-runs` | Where run directories go. Relative paths start from the agent's working directory |

The agent does not serve TLS; remote clients need an SSH tunnel or TLS proxy.

Check the agent. `/health` needs no token:

```sh
POLKAMETER_AGENT_TOKEN=docs-demo-token polkameter agent serve --bind 127.0.0.1:9901 &
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
| `POST /runs/{id}/stop` | Stops a running run |

The client checks each run ID: at most 128 characters, from letters, digits, `-`, `_` and `.`.

- **Transport.** The client accepts `https://`, or `http://` only for `127.0.0.1`, `localhost` or `[::1]` with an explicit port.
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
| `130` | Stopped | Ctrl-C (SIGINT) during a local run, or a stopped remote run |

A `run` never exits with `3`. A preflight failure inside a run makes it `failed`, with exit code 1.

By default, a stress-mode load run exits `0` whatever its verdict. Add `--fail-on-verdict` to exit `1` on `FAIL`. This uses the existing failed-run code; `WARN` and `INCONCLUSIVE` do not trigger it. See [reading the results](reading-results.md).

## Environment variables

| Variable | Read by | Effect |
| --- | --- | --- |
| `POLKAMETER_PLUGIN_REGISTRY` | Commands that use installed plugins | Path of the plugin registry (JSON). Default: `~/.config/polkameter/plugins.json` |
| `HOME`, `USERPROFILE` | The default registry path | Home directory for the default path. `USERPROFILE` is used if `HOME` is unset. If neither is set, set `POLKAMETER_PLUGIN_REGISTRY` |
| `POLKAMETER_NODE_PID` | Load runs with `<monitors>` | The node's process ID, for CPU and memory. If unset, PolkaMeter finds the process on the RPC port with `lsof`, then `ss`. If none is found, no CPU or memory is recorded |
| `POLKAMETER_AGENT_TOKEN` | `agent serve` | Default name of the token variable, used when `--token-env` is not given |

Plugins start with a cleared environment. Only `PATH`, `TMPDIR`, `TEMP`, `SYSTEMROOT`, `SSL_CERT_FILE` and `SSL_CERT_DIR` are passed on. A plugin receives credential values only as step inputs.

## Output formats

With `--format json`, `validate`, `preflight`, `run` and `report` print one JSON object per line on standard output.

```sh
polkameter validate examples/plugin-workflow.polkameter.xml --format json
```

```json
{"event":"validation","plan":"Plugin data flow","valid":true}
```

- **Errors** go to standard error as `Polkameter: <message>`, even with `--format json`.
- **Load runs** also print `stop:`, `loss check:` and `smoke:` lines, and the summary, to standard error.

The CLI adds a final line: `artifact-written` for `run`, `validation` for `validate`, `report` for `report`, with the summary text. A remote run prints only `artifact-written`, because the agent does not stream events. Fields are in [events.jsonl](results.md#eventsjsonl).
