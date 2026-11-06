# Run on a remote machine

Use a remote agent when the plugins, credentials or network access you need exist only on another machine. The agent runs the plan on its own host. Your machine sends the plan's XML and gets back the path of the run directory on that host.

## Start the agent

On the remote host, install the plugins and map the credential profiles the plan uses, with `polkameter plugin install` and `polkameter plugin credential`. The agent reads only this host's registry.

Generate a random token, then start the agent:

```sh
export POLKAMETER_AGENT_TOKEN=$(openssl rand -hex 32)
polkameter agent serve --bind 127.0.0.1:9901 --output-root /data/runs --keep-runs 50
```

- `--bind` (default `127.0.0.1:9901`) must be a loopback address. The agent does not serve TLS.
- `--token-env` (default `POLKAMETER_AGENT_TOKEN`) names the variable that holds the token. It must be set, at least 32 characters long and use at least 8 different characters. There is no flag for the token itself.
- `--output-root` (default `target/polkameter-agent-runs`) is where each run's `run-…` directory is created.
- `--keep-runs` (default: keep all) keeps the newest finished run directories. When a run starts, older finished `run-…` directories under `--output-root` are deleted. The previous run always survives the next start, so its client can still fetch its report. Runs in progress are never deleted. Give each agent its own `--output-root`.

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

You do not need `--output` with `--remote`. The CLI validates the plan first, so a bad plan fails before any request. `--remote` takes `https://`, or `http://` to a loopback host through a tunnel. It refuses an address with a user name or password in it, and it never follows a redirect, so the token goes only to the address you give. `--credential-env` cannot be combined with `--remote`.

When the run ends, the CLI prints the run directory's path on the agent host and exits with the run's exit code:

```text
Remote run completed. Artifacts: .../run-1792254490330-1974-2
```

## Get the results

The client never receives the run's files. They stay in the run directory under `--output-root` on the agent host, with the same files as a local run. Copy the directory to read it, or run `polkameter report <run-dir>` on the agent host.

If the connection to the agent drops, the client keeps polling with backoff for 60 seconds before it gives up:

```text
Polkameter: status poll failed (remote runner request failed: error sending request for url (http://127.0.0.1:9901/runs/c-5f0c2a9e41d7b38e6a0f1d2c3b4a5968): client error (Connect): tcp connect error: Connection refused (os error 61)); retrying in 600 ms
...
Polkameter: status polls kept failing for 60 s: ...; asked the agent to stop run c-5f0c2a9e41d7b38e6a0f1d2c3b4a5968
```

The client chooses the run's ID and sends it with the start request. If the start request gets no answer, for example because a proxy dropped the connection, the client sends it again with the same ID, and the agent does not start a second run.

Whenever the client gives up, it first asks the agent to stop the run, so the agent is free for the next one. The client also gives up when the run lasts longer than the plan's `timeout-ms`, plus its teardown step timeouts, plus one minute, counted from before the start request. It then exits with code `1`.

## Stop a run

Press Ctrl-C (SIGINT) in the client, or send it SIGTERM. The client asks the agent to stop the run and waits for the outcome. The agent runs the plan's teardown, writes the run directory and records the state `stopped`. The client exits with `130` for SIGINT or `143` for SIGTERM:

```text
Polkameter: received SIGINT; asking the agent to stop the run (teardown has up to 20 s; send the signal again to stop now)
Remote run stopped. Artifacts: /data/runs/run-1793933833445-39378-2
```

A second signal, or the end of `--shutdown-grace` (20 seconds by default), asks the agent to interrupt teardown at once, even while the first stop request is still unanswered. Each stop request may take at most 5 seconds. From the forced stop, the client waits at most 10 seconds for the outcome, then exits with the signal's code; the run keeps stopping on the agent.

The agent runs one plan at a time. A second start fails with `a plugin plan is already running` until the first run ends.

## Stop the agent

Send the agent SIGINT or SIGTERM, as `systemctl stop` or `docker stop` do. The agent refuses new runs and stops the active run, which still runs its teardown within the agent's `--shutdown-grace`. The run's client receives the stopped outcome. The agent then exits with `130` for SIGINT or `143` for SIGTERM:

```text
Polkameter: received SIGTERM; stopping the active run (teardown has up to 20 s; send the signal again to stop now)
```

A second signal interrupts teardown. Give the service manager `--shutdown-grace` plus 10 seconds before it sends SIGKILL; see [stop a run](cli.md#stop-a-run) for common settings.

## What the agent refuses

- A bind address that is not loopback.
- A token shorter than 32 characters, or with fewer than 8 different characters. The agent does not start.
- A request with no bearer token, or a wrong one (`401`).
- A plan that names a plugin the agent does not have. Clients cannot upload plugins (`400`).
- A second run while one is running (`400`).
- A new run while the agent is shutting down (`503`).
- A preflight step that can change state.
- A plain `http://` address that is not loopback, or any address with a user name or password. The client refuses it before sending anything.

Installed plugins run as trusted code, without a sandbox. The bearer token grants full trust on the agent: every plugin, credential profile and topology there, and any endpoint a plan names. Use one agent and one token for each trust domain.
