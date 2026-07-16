# Getting started

This path creates a scenario, validates it without a chain, then runs it against a fresh local development node. It uses the CLI because it is the smallest reproducible surface; the desktop app can create the same scenario file.

## Prerequisites

- A supported desktop platform for the app, or a Rust-capable host for the CLI.
- Node.js 22 and Corepack/pnpm for frontend or desktop development.
- Rust **1.93.0** for the project source.
- A Polkadot SDK node when preflighting or executing. The examples use a local `polkadot` development node.

For development dependencies and build commands, see [Install and build](install.md). For an existing release, use the bundled `polkameter` executable; its name is intentionally separate from the desktop binary (`polkameter-desktop`).

## The shortest safe workflow

1. Save a portable XML plan. Start from `src-tauri/tests/fixtures/valid-scenario.polkameter.xml` or create one in the desktop app.
2. Check its structure locally: `polkameter validate scenario.polkameter.xml`.
3. Start a disposable local chain and set a signer only in the process environment.
4. Preflight. It connects to the chain and validates metadata and call encoding, but makes no submission.
5. Run to a dedicated output directory and read the generated report.

The [first local run](first-run.md) provides copyable commands and explains each output.

## Choose an interface

| Need | Use |
|---|---|
| Create, edit, save, or visually monitor a plan | [Desktop application](using/desktop.md) |
| Run in CI, scripts, containers, or a terminal | [CLI](using/cli.md) |
| Execute on a dedicated machine without sending it a secret | [Remote runner agent](using/remote-agent.md) |

All three use the same `.polkameter.xml` plan. Keep that file in version control, and keep the corresponding SURI in a credential vault or CI secret instead. Legacy JSON plans remain readable for compatibility, but the desktop saves XML and new plans should use XML.
