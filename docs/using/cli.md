# Command-line interface

The `polkameter` executable is a headless Polkadot SDK stress-testing command. It opens no window and is appropriate for shell scripts, CI jobs, and stress hosts.

## Commands

| Command | What it does |
|---|---|
| `polkameter validate <scenario>` | Parses and structurally validates a scenario without contacting a chain. |
| `polkameter preflight <scenario>` | Resolves a signer and validates live metadata, SCALE encoding, and readiness without submitting. |
| `polkameter run <scenario> --output <dir>` | Preflights and executes locally, writing artifacts under `<dir>`. |
| `polkameter run <scenario> --remote <url>` | Preflights and executes through an authenticated remote agent. The agent owns the artifacts. |
| `polkameter report <artifact-dir>` | Validates a portable artifact directory and prints its report. |
| `polkameter agent serve` | Starts a loopback-only authenticated remote runner agent. |

Every command accepts `--format human` (the default) or `--format json`. Run `polkameter <command> --help` for the current flags.

## Local execution

```sh
# No RPC connection; useful as a fast CI gate.
polkameter validate scenario.polkameter.json

# Use a local credential-vault profile.
polkameter preflight scenario.polkameter.json --signer-profile local-dev

# Or resolve the SURI from the named environment variable.
POLKAMETER_SURI='//Alice' \
  polkameter run scenario.polkameter.json \
  --signer-env POLKAMETER_SURI \
  --output target/polkameter-runs

polkameter report target/polkameter-runs/run-EXAMPLE
```

`--signer-profile` and `--signer-env` are mutually exclusive. `--output` is required for a local run and is not used for a remote run.

## Output streams

Human `validate`, `preflight`, and `report` results go to standard output. A human `run` reserves standard output for the final artifact location/report summary and writes progress and individual sample failures to standard error.

With `--format json`, Polkameter writes versioned JSON Lines events to standard output. This keeps machine-readable results separate from diagnostics. Treat the final `artifact-written` event as the terminal success/failure artifact notice. Full stream and exit-code details are in [CLI exit codes and streams](../reference/cli-contract.md).

## Interrupting a run

Send `Ctrl-C` once to request a graceful stop. Polkameter stops scheduling further work, lets active watches finish or time out within `shutdownDrainTimeoutMs`, writes the artifact bundle, and exits with `130` when interrupted.
