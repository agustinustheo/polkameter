---
title: Getting started
description: Build Polkameter, install the example plugin and run the example plan
---

This page builds Polkameter, installs the example plugin and runs the example plan. The example needs no chain.

## Prerequisites

- Rust 1.93 or newer. CI pins 1.93.0.
- For the desktop app only: Node 22 and pnpm. `package.json` pins pnpm 10.32.1, and `corepack pnpm` uses that version. The desktop app also needs the Tauri 2 system dependencies for your platform.

## 1. Build

The CLI builds without the desktop app, so it needs neither Node nor a frontend build:

```sh
cargo build -p polkameter --no-default-features --bin polkameter
cargo build -p polkameter-example-plugin
```

This produces `target/debug/polkameter` (the CLI) and `target/debug/polkameter-example-plugin`. For a release CLI, add `--release` and use `target/release/` in the paths below.

The desktop app is the `desktop` feature of the same package, enabled by default. It embeds the frontend, so build the frontend first. Step 7 has the commands.

## 2. Install the example plugin

Plugins are recorded in a registry file. For this walkthrough, keep it inside `target/`:

```sh
export POLKAMETER_PLUGIN_REGISTRY="$PWD/target/plugins.json"
target/debug/polkameter plugin install target/debug/polkameter-example-plugin
target/debug/polkameter plugin list
```

`plugin install` reads the plugin's manifest and records its version and the BLAKE2 hash of its executable. Reinstall after rebuilding the plugin, because the hash changes. Without `POLKAMETER_PLUGIN_REGISTRY`, the registry is `~/.config/polkameter/plugins.json`.

## 3. Validate, inspect and run the example plan

```sh
target/debug/polkameter validate examples/plugin-workflow.polkameter.xml
target/debug/polkameter plugin inspect examples/plugin-workflow.polkameter.xml
target/debug/polkameter run examples/plugin-workflow.polkameter.xml --output target/runs
```

- `validate` parses and checks the plan. It does not connect to a chain or start plugins.
- `plugin inspect` resolves the installed manifests against the plan and prints what it needs. It does not connect to a chain or submit anything.
- `run` executes the plan and writes a new run directory under `target/runs`.

The example plan has a setup step and one workflow with 3 users and 2 iterations. Setup calls `example.double` on 21, which returns 42, and copies the result with `core.echo`. Each workflow iteration doubles 42 to 84 and asserts that the value equals 84. A failed assertion makes the run fail with exit code 1.

The full plan is in [`examples/plugin-workflow.polkameter.xml`](https://github.com/agustinustheo/polkameter/blob/main/examples/plugin-workflow.polkameter.xml). [Plans]({{ '/plans.html' | relative_url }}) explains each element.

## 4. Read the run directory

The run directory is `target/runs/run-<timestamp>-<pid>-<sequence>/`. For a plan without a `<load>`, it holds:

| File | Contents |
| --- | --- |
| `plan.json` | The plan as parsed |
| `resolved-plan.json` | The installed plugin manifests and checksums, and the CLI's own hash |
| `events.jsonl` | Every event of the run, one JSON object per line |
| `execution.json` | The outcome: `state`, `exit_code` and `error` |
| `samples.jtl` | A JMeter CSV with one row per finished step |
| `summary.md` | A short status: the plan name, the state and any error |
| `plugins/example/stderr.log` | The plugin's standard error |

A plan with a `<load>` adds the measurement files, `summary.json` and `transactions.jsonl`. Every run has plots. [Results and verdicts]({{ '/results.html' | relative_url }}) lists them all.

## 5. Regenerate the report

`polkameter report` rebuilds a run's outputs from the files in a run directory. For a run with a `<load>`, it rebuilds the checks, `summary.json`, `summary.md`, `samples.jtl` and the plots. For a run without one, it rewrites `samples.jtl` and the plots:

```sh
target/debug/polkameter report target/runs/run-<timestamp>-<pid>-<sequence>
```

Use it on a copied run directory to reproduce a result on another machine.

## 6. Run the smoke test

`tests/cli-smoke.sh` runs the CLI end to end without a chain. It validates and runs the example plan locally, regenerates its report, checks that a failed assertion exits nonzero, and runs the plan through a loopback remote agent. It uses the binaries from step 1:

```sh
tests/cli-smoke.sh
```

## 7. Desktop app (optional)

The desktop app needs the frontend built before the Rust build embeds it:

```sh
pnpm install --frozen-lockfile
pnpm build
pnpm tauri dev
```

## Next steps

- Run a plan against a live Zombienet network with [`scripts/local-fork-run.sh`](https://github.com/agustinustheo/polkameter/blob/main/scripts/local-fork-run.sh). The [plugins guide]({{ '/plugins.html' | relative_url }}#running-on-a-local-network) describes its environment variables.
- Read [Architecture]({{ '/architecture.html' | relative_url }}) to see how the crates fit together.
