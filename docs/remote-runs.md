# Run on a remote machine

Use a remote agent when the plugins, credentials or network access you need exist only on another machine. The agent runs the plan on its own host. Your machine sends the plan's XML and gets back the path of the run directory on that host.

## Start the agent

On the remote host, install the plugins and map the credential profiles the plan uses, with `polkameter plugin install` and `polkameter plugin credential`. The agent reads only this host's registry.

Set a long random token, then start the agent:

```sh
export POLKAMETER_AGENT_TOKEN=<token>
polkameter agent serve --bind 127.0.0.1:9901 --output-root /data/runs
```

- `--bind` (default `127.0.0.1:9901`) must be a loopback address. The agent does not serve TLS.
- `--token-env` (default `POLKAMETER_AGENT_TOKEN`) names the variable that holds the token. It must be set and not empty. There is no flag for the token itself.
- `--output-root` (default `target/polkameter-agent-runs`) is where each run's `run-…` directory is created.

To reach a loopback agent from another machine, forward its port over SSH:

```sh
ssh -N -L 9901:127.0.0.1:9901 <user>@<remote-host>
```

## Run the plan from your machine

Set the same token on your machine, and pass the name of that variable with `--remote-token-env`:

```sh
export AGENT_TOKEN=<the same token>
polkameter run plans/transfers.polkameter.xml --remote http://127.0.0.1:9901 --remote-token-env AGENT_TOKEN
```

You do not need `--output` with `--remote`. The CLI validates the plan first, so a bad plan fails before any request. `--remote` takes `https://`, or `http://` to a loopback host through a tunnel. `--credential-env` cannot be combined with `--remote`.

When the run ends, the CLI prints the run directory's path on the agent host and exits with the run's exit code:

```text
Remote run completed. Artifacts: .../run-1792254490330-1974-2
```

## Get the results

The client never receives the run's files. They stay in the run directory under `--output-root` on the agent host, with the same files as a local run. Copy the directory to read it, or run `polkameter report <run-dir>` on the agent host.

## Stop a run

Press Ctrl-C in the client. The client asks the agent to stop the run, waits until the run ends and exits with code `130`. The run's `execution.json` records the state `stopped`.

The agent runs one plan at a time. A second start fails with `a plugin plan is already running` until the first run ends.

## What the agent refuses

- A bind address that is not loopback.
- A request with no bearer token, or a wrong one (`401`).
- A plan that names a plugin the agent does not have. Clients cannot upload plugins (`400`).
- A second run while one is running (`400`).
- A preflight step that can change state.
- A plain `http://` address that is not loopback. The client refuses it before sending anything.

Installed plugins run as trusted code, without a sandbox. The bearer token grants full trust on the agent: every plugin, credential profile and topology there, and any endpoint a plan names. Use one agent and one token for each trust domain.
