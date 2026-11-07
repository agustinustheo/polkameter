# Run on a remote machine

Use an agent to run the plan on another machine. Install its plugins and configure its credentials there. Your machine sends only the plan's XML, and the agent keeps the run directory.

## Start the agent

On the remote host, install the plugins the plan uses with `polkameter plugin install`. Map its credential profiles with `polkameter plugin credential`. The agent reads only this host's registry.

Generate a random token, then start the agent:

```sh
export POLKAMETER_AGENT_TOKEN=$(openssl rand -hex 32)
polkameter agent serve --bind 127.0.0.1:9901 --output-root /data/runs --keep-runs 50
```

The agent doesn't serve TLS. Once it listens, it prints `PolkaMeter: agent listening on <address>`. With port `0`, the system picks a free port, and that line names it.

`--token-env` names the variable that holds the token. There's no flag for the token itself.

`--keep-runs` keeps the newest finished run directories under `--output-root` and deletes older ones when a run starts. Runs in progress are never deleted. Give each agent its own `--output-root`.

To reach the agent from another machine, forward its port over SSH:

```sh
ssh -N -L 9901:127.0.0.1:9901 <user>@<remote-host>
```

## Run the plan from your machine

Set the same token on your machine. Pass the name of that variable with `--remote-token-env`:

```sh
export AGENT_TOKEN=<the same token>
polkameter run plans/transfers.polkameter.xml --remote http://127.0.0.1:9901 --remote-token-env AGENT_TOKEN
```

You don't need `--output` with `--remote`, and you can't use `--credential-env`. The client never follows a redirect, so the token goes only to the address you give.

When the run ends, the CLI prints the run directory's path on the agent host. It exits with the run's exit code:

```text
Remote run completed. Artifacts: .../run-1792254490330-1974-2
```

## Get the results

Run files stay on the agent. Use `--junit PATH` to download a local copy of `junit.xml`, or copy the run directory yourself. You can also run `polkameter report <run-dir>` on the agent host.

If the client can't reach the agent for 60 seconds, it asks the agent to stop the run and exits `1`. It does the same when the run lasts longer than the plan's `timeout-ms`, plus its teardown step timeouts, plus one minute.

## Stop a run

Press Ctrl-C (SIGINT) in the client, or send it SIGTERM. The client asks the agent to stop the run and waits for the outcome. The agent runs the plan's teardown and records the state `stopped`. The client exits `130` for SIGINT or `143` for SIGTERM:

```text
PolkaMeter: received SIGINT; asking the agent to stop the run (teardown has up to 20 s; send the signal again to stop now)
Remote run stopped. Artifacts: /data/runs/run-1793933833445-39378-2
```

A second signal, or the end of `--shutdown-grace`, asks the agent to interrupt teardown. See [Stop a run](cli.md#stop-a-run) for the timing.

## Stop the agent

Send the agent SIGINT or SIGTERM, as `systemctl stop` does. The agent refuses new runs and stops the active run. That run still gets its teardown within the agent's `--shutdown-grace`. Then the agent exits `130` or `143`:

```text
PolkaMeter: received SIGTERM; stopping the active run (teardown has up to 20 s; send the signal again to stop now)
```

A second signal interrupts teardown. Give the service manager `--shutdown-grace` plus 10 seconds before it sends SIGKILL.

## What the agent refuses

- A bind address that is not loopback.
- A token shorter than 32 characters, or with fewer than 8 different characters. The agent does not start.
- A request with no bearer token, or a wrong one (`401`).
- A plan that names a plugin the agent does not have (`400`). Clients cannot upload plugins.
- A second run while one is running (`400`). The agent runs one plan at a time.
- A new run while the agent is shutting down (`503`).
- A preflight step that can change state.
- A plain `http://` address that is not loopback, or any address with a user name or password. The client refuses it before it sends anything.

Installed plugins run as trusted code, without a sandbox. The token grants full trust on the agent: every plugin, credential profile and topology there, and any endpoint a plan names. Give each group that may use the same plugins and credentials its own agent and token.
