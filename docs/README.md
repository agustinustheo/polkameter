# Polkameter

<img src="logo-light.png" class="logo-light" alt="Polkameter" />
<img src="logo-dark.png" class="logo-dark" alt="Polkameter" />

Polkameter is a JMeter-inspired stress-testing workbench for **Polkadot SDK chains**. It lets a team define a repeatable transaction test plan, validate that plan against the live runtime, then run it with controlled concurrency and inspect portable artifacts afterwards.

It is deliberately two interfaces over one execution core:

- The **Tauri desktop application** is for composing plans, storing local signer profiles, previewing schedules, watching a run, and opening its report.
- The headless **`polkameter` CLI** is for terminals, CI jobs, and dedicated stress machines. It uses the same scenario format, preflight, scheduler, signer derivation, runner, and report contract as the desktop app.

## What Polkameter does

1. Models load as thread groups, ordered setup/transaction/teardown samplers, and one of three arrival models.
2. Connects to a WebSocket RPC during preflight, reads its current metadata, and SCALE-encodes every selected pallet call before anything is submitted.
3. Derives distinct virtual-user accounts from a local signer source and run ID, verifies they are ready, then submits under group and plan-wide concurrency limits.
4. Writes a redacted, portable artifact bundle with JTL samples, event and telemetry logs, a Markdown summary, and SVG plots.

Polkameter is chain-generic at the execution boundary: it currently runs the standard `PolkadotConfig` transaction profile. The pallet, call, and arguments are supplied by the scenario and verified against the target chain at preflight time.

## Read this first

- Start with [Getting started](getting-started.md) if you want a local run.
- Use [XML test plans](using/scenarios.md) to author a durable `.polkameter.xml` plan.
- Use [CI/CD integration](operations/ci.md) for a safe automation pattern.
- Read [Signers and security](using/signers.md) before putting a signer on any machine.

## Safety boundary

Previewing, validating, and preflighting do not submit transactions. A run does. Test on a disposable development chain first, use dedicated funded accounts, and understand the target chain's fee and operational limits before applying real load.
