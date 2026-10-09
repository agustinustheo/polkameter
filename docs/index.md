---
layout: home
title: Polkameter
description: A JMeter-inspired load and stress-testing workbench for Polkadot SDK chains
---

Polkameter is a load and stress-testing tool for Polkadot SDK chains, modeled on JMeter. An XML plan describes the work: the steps, the users and iterations, the load profile, the monitors and the checks. Installed plugins supply the parts that depend on the runtime, such as how to build a transaction, what state it should leave behind, and which pallets to watch. The engine is written in Rust and knows no particular runtime.

## What one run does

1. **Validate.** The plan is parsed and checked without connecting to a chain: IDs, references, limits and the shape of every step.
2. **Preflight.** The installed plugins are checked against the plan and read-only steps run. For a load, the block interval measured on the chain is compared with the one the plan expects.
3. **Setup and workflows.** Setup steps run once. Workflows run with bounded users, iterations and concurrency.
4. **Measure.** A baseline of probe transactions runs first, then stepped or ramped load until a stop rule fires or the plan ends, then recovery probes. Every submitted transaction is reconciled against the finalized chain and the ready pool, and receives exactly one status.
5. **Judge.** Evaluate steps and the outcome checks read the recorded series and produce verdicts.
6. **Report and clean up.** The report is written, teardown steps run, and plugins shut down.

Each run writes a run directory with a JMeter-compatible `samples.jtl`, an event log, plots and a short `summary.md`. A run with a load adds the raw recordings and `summary.json`, and its `summary.md` holds the full verdict and checks. [Results and verdicts]({{ '/results.html' | relative_url }}) explains every file.

## Three front ends, one engine

- **Command line.** The `polkameter` binary validates, preflights and runs plans, and regenerates reports. It is meant for CI and for remote load machines.
- **Desktop app.** A Tauri application that opens, edits and runs the same plans with the same engine.
- **Remote agent.** `polkameter agent serve` exposes the engine over an authenticated HTTP API on a machine that has its own plugins and credentials. A client sends the plan XML and collects the outcome.

## Read next

- [Getting started]({{ '/getting-started.html' | relative_url }}): build Polkameter, install the example plugin and run the example plan.
- [Architecture]({{ '/architecture.html' | relative_url }}): the crates, what each one owns, and how a run flows through them.
- [Plans]({{ '/plans.html' | relative_url }}): the XML reference, references between steps, and validation rules.
- [Results and verdicts]({{ '/results.html' | relative_url }}): the run directory, stop rules, checks and exit codes.
- [CLI]({{ '/cli.html' | relative_url }}): every command and flag, and the remote agent.
- [Plugins]({{ '/plugins.html' | relative_url }}): writing and installing plugins, and the plugin protocol.
- [Slides: Polkameter in 15 minutes]({{ '/slides/' | relative_url }})

The source is on [GitHub](https://github.com/agustinustheo/polkameter) and is licensed under Apache-2.0.
