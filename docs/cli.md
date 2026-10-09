---
title: CLI
description: Every polkameter command and flag, the remote agent, and its security rules
---

The `polkameter` binary validates, preflights and runs plans, reports on finished runs, manages plugins on the host, and serves a remote agent. Run `polkameter --help` for the command list, or `polkameter COMMAND --help` for one command.

## Commands

| Command | Purpose |
| --- | --- |
| [`plugin`](#plugin) | Install plugins, map credentials and topologies, and record capabilities on this host |
| [`validate`](#validate) | Parse and check a plan without connecting to a chain |
| [`preflight`](#preflight) | Check the installed plugins and the chain, and run the read-only preflight steps |
| [`run`](#run) | Run a plan locally, or through a remote agent |
| [`report`](#report) | Rebuild the checks, summaries and plots of a run directory |
| [`agent serve`](#agent-serve) | Serve remote runs on this host |

Options that take a value are written `--flag VALUE`. `--format` takes `human` (the default) or `json`.

## Output format

With `--format json`, `validate`, `preflight`, `run` and `report` print one JSON object per line, on standard output. The human format prints a short line, and `run` also prints its phases to standard error.

Events of a local run are the same objects written to the run's `events.jsonl`. Each has a `version` (2) and a `timestamp` in milliseconds:

| Event | Fields | Meaning |
| --- | --- | --- |
| `phase` | `phase`, and `id` in a workflow | The run entered a phase: `preflight`, `setup`, `workflow`, `measurement`, `baseline`, `load`, `recovery`, `reconciliation`, `checks` or `teardown` |
| `step-started` | `step`, `operation`, `user`, `iteration` | A step began. `user` and `iteration` are null outside a workflow |
| `step-progress` | `step`, `elapsedMs` | A long step is still running |
| `step-finished` | `step`, `user`, `iteration`, `elapsedMs`, `success`, `error` | A step ended. `error` is null on success |
| `cleanup-failed` | `error` | A teardown step failed |
| `completed` | `outcome` | The run ended |

The CLI adds its own final line. A local run prints `artifact-written` with the outcome. A remote run prints only that line, because the agent does not stream events. `validate` prints `validation`, and `report` prints `report` with the summary text.

```json
{"event":"validation","valid":true,"plan":"Plugin data flow"}
```

## plugin

```sh
polkameter plugin install EXECUTABLE
polkameter plugin list
polkameter plugin inspect SCENARIO
polkameter plugin credential PROFILE ENV
polkameter plugin topology NAME PATH
polkameter plugin capability NAME
```

Plugin commands always print pretty JSON.

| Subcommand | Arguments | Effect |
| --- | --- | --- |
| `install` | `EXECUTABLE`: the plugin executable | Reads the plugin's manifest and records its ID, version and the BLAKE2 hash of the executable. Reinstall after rebuilding the plugin |
| `list` | none | Lists the installed plugins |
| `inspect` | `SCENARIO`: a plan file | Starts the plan's plugins, checks each operation against its manifest, and prints the manifests, the evidence the plugins require, the plan's targets, the monitor requirements and the node metric catalog. It does not connect to a chain or submit anything |
| `credential` | `PROFILE`, `ENV`: the environment variable | Maps a credential profile named in a plan to an environment variable on this host |
| `topology` | `NAME`, `PATH`: the topology file. The plan refers to it by `NAME` | Registers a Zombienet `zombie.json` under an alias. The plan names the alias, and the path is stored as an absolute path |
| `capability` | `NAME` | Records a capability that the network provisioner supplies. A plugin can require it as evidence |

The registry is a JSON file. It defaults to `~/.config/polkameter/plugins.json`, and the `POLKAMETER_PLUGIN_REGISTRY` environment variable overrides the path. On Windows, `USERPROFILE` is used if `HOME` is not set.

## validate

```sh
polkameter validate PLAN [--format human|json]
```

Parses the plan and checks its structure: IDs, references, limits and the shape of each step. It does not start plugins or connect to a chain. An invalid plan exits with code 2. [Plans]({{ '/plans.html#validation-rules' | relative_url }}) lists the rules.

## preflight

```sh
polkameter preflight PLAN [--credential-env ENV_VAR] [--format human|json]
```

Starts the plan's plugins and checks each operation against its manifest. It then checks the evidence the plugins require, such as a capability, an RPC method or a metric, and runs the plan's read-only preflight steps against the chain. For a plan with a load, it also measures the block interval. It exits with code 3 when preflight fails.

| Flag | Meaning |
| --- | --- |
| `--credential-env ENV_VAR` | Reads the plan's credential from this environment variable instead of the profile on this host. The plan must declare exactly one credential |

## run

```sh
polkameter run PLAN --output DIR [--credential-env ENV_VAR] [--format human|json]
polkameter run PLAN --remote URL --remote-token-env ENV_VAR [--format human|json]
```

Runs the plan. A local run writes a new run directory under `--output`. A remote run sends the plan's XML to an agent, which runs it on its own host, with its own plugins and credentials.

| Flag | Required | Meaning |
| --- | --- | --- |
| `--output DIR` | Yes, unless `--remote` is given | The directory the run directory is created in |
| `--credential-env ENV_VAR` | No | As for `preflight`. Cannot be combined with `--remote` |
| `--remote URL` | No | The agent's endpoint: `https://`, or `http://` to a loopback address through a tunnel. See [remote agent](#remote-agent) |
| `--remote-token-env ENV_VAR` | With `--remote` | The environment variable that holds the agent's bearer token |

Ctrl-C stops a local run. The run writes what it has and exits with code 130. With `--remote`, Ctrl-C asks the agent to stop the run, then waits for its outcome.

The exit codes are in [results]({{ '/results.html#exit-codes' | relative_url }}).

## report

```sh
polkameter report ARTIFACT_DIRECTORY [--format human|json]
```

Rebuilds the run's outputs from its files. For a run with a `<load>`, it rebuilds the built-in and plugin checks, `summary.json`, `summary.md`, `samples.jtl` and the plots under `plots/`. For a run without one, it rewrites `samples.jtl` and the plots. The directory must hold `execution.json` and `summary.md`. The human format prints the summary. It exits with code 0 when it succeeds.

## agent serve

```sh
polkameter agent serve [--bind ADDR] [--token-env ENV_VAR] [--output-root DIR]
```

Starts the remote agent on this host.

| Flag | Default | Meaning |
| --- | --- | --- |
| `--bind ADDR` | `127.0.0.1:9901` | The address to listen on. It must be a loopback address |
| `--token-env ENV_VAR` | `POLKAMETER_AGENT_TOKEN` | The environment variable that holds the bearer token. It must be set and not empty |
| `--output-root DIR` | `target/polkameter-agent-runs` | Where the agent creates the run directories |

The agent refuses to start on a non-loopback address. Put it behind an SSH tunnel or a TLS-terminating proxy if clients need to reach it from another host.

## Remote agent

Every route except `/health` requires `Authorization: Bearer TOKEN`, and a missing or wrong token gets `401 Unauthorized`. Requests and responses are JSON. A plan is sent as `{"xml": "..."}`.

| Method and path | Purpose |
| --- | --- |
| `GET /health` | Liveness. Returns `{"status":"ok"}` and needs no token |
| `GET /plugins` | The protocol version and the installed plugins, with their versions and hashes |
| `POST /inspect` | The same check as `plugin inspect`, for a plan |
| `POST /preflight` | The same check as `preflight`, for a plan |
| `POST /runs` | Starts a run. Returns its status, including the run ID |
| `GET /runs/{id}` | The run's state, phase and, once it ends, its outcome |
| `POST /runs/{id}/stop` | Stops a running run |

The client accepts only run IDs made of letters, digits, `-`, `_` and `.`, up to 128 characters, before it sends them.

Security rules:

- **Transport.** A client accepts an `https://` endpoint, or an `http://` endpoint only when the host is `127.0.0.1`, `localhost` or `[::1]`, with an explicit port. The loopback case is for an SSH tunnel. Any other plain-HTTP endpoint is refused.
- **Trust.** The bearer token grants full execution trust on that agent: every credential profile, topology and plugin configured there can be used by any plan that holds the token, and a plan can name any endpoint in its `<targets>`. Use a separate agent, and a separate token, for each trust domain.
- **What crosses the wire.** A client sends plan XML. It never sends executable paths or secret values. Plugins, credentials and topologies are configured on the agent, with `polkameter plugin`.
- **Plugins.** Installed plugins are trusted code and are not sandboxed.
- **Artifacts.** A remote run's files stay on the agent, under its `--output-root`. The final line of `run --remote` gives the path on the agent.

A worker and a client, sharing one token:

```sh
# On the worker. The agent reads its token from POLKAMETER_AGENT_TOKEN:
polkameter agent serve --bind 127.0.0.1:9901 --output-root /data/runs

# On the client, through an SSH tunnel to the worker's port 9901.
# AGENT_TOKEN holds the same token as the worker's POLKAMETER_AGENT_TOKEN:
polkameter run scenario.xml --remote http://127.0.0.1:9901 --remote-token-env AGENT_TOKEN
```
