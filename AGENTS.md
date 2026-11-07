# AGENTS.md

Instructions for AI coding agents working on the PolkaMeter repository. To use PolkaMeter in another project, see the skill in [`skills/polkameter`](skills/polkameter/SKILL.md) instead.

## What this is

PolkaMeter is a load-testing tool for Polkadot SDK chains. The `polkameter` CLI runs XML plans. Plugins are separate executables that prepare chain-specific transactions. Plugins exchange one JSON message per line with the CLI through standard input and output.

## Layout

| Path | What it is | Published |
| --- | --- | --- |
| `crates/cli` | The `polkameter` CLI and remote agent. Plan engine in `src/engine`, load runner in `src/load` | crates.io `polkameter` |
| `crates/plugin-sdk` | `polkameter-plugin-sdk`: protocol (default), `chain` feature (RPC, reads, signing, keys), `observers` feature (run files, checks) | crates.io `polkameter-plugin-sdk` |
| `plugins/substrate` | The substrate plugin, `polkameter-substrate-plugin`. Needs the standard `System` and `Balances` pallets and supported account and signature types | release archives and container only |
| `plugins/example` | Example plugin for tests. No chain | no |
| `src-tauri`, `src` | Desktop app (Tauri, TypeScript). Not documented for users | no |
| `examples/` | Example plans. `tests/validate-plans.py` validates every `*.polkameter.xml` here | |
| `schemas/` | The plan XSD | |
| `docs/` | mdBook user docs. Read `docs/STYLE.md` before you edit them | |
| `scripts/` | Release, packaging, action and dev-chain scripts | |
| `skills/polkameter` | Agent Skill for users who set up PolkaMeter. Its plan templates are copies of `examples/` | |
| `.claude-plugin` | Claude Code marketplace and plugin manifest. The plugin is the repository root | |

`polkameter-plugin-sdk` depends on no other PolkaMeter crate. `polkameter` depends on the SDK with every feature on. The engine and load modules are public only for tests. They are not a library API.

## How a run flows through the code

1. `engine::execute::run` validates the plan, creates the run directory and writes events to `events.jsonl`.
2. `engine::plugins` starts one process per plugin. Each step sends an operation over the SDK protocol.
3. For a load test, `engine::measurement::run` starts the monitors (`engine/monitors`) and the load runner (`load`).
4. After the finality wait, `load::loss` gives each flood transaction its status.
5. `observers::checks::report::write` reads the files through `observers::files`, runs the checks and writes `summary.json` and `summary.md`.
6. `engine::artifacts` writes `samples.jtl` and the plots. `execute` writes `execution.json`.

Each part writes its own raw files. Only `observers::files` knows the formats. That's why `polkameter report` can run steps 5 and 6 again from the files alone.

Where to change things:

- New check: add its metric to `crates/plugin-sdk/src/observers/files/registry.rs`, record it in a monitor, and add the check under `crates/plugin-sdk/src/observers/checks/outcomes/`.
- Stop rule or default threshold: `crates/cli/src/load/rules.rs`.
- Check limit: `crates/plugin-sdk/src/observers/checks/limits.rs`.
- Plan rules: `crates/cli/src/engine/plan.rs` and `xml.rs`, and the XSD in `schemas/`. `tests/validate-plans.py` runs every example and documentation plan through both xmllint and `polkameter validate`.

## Build and test

Use Rust 1.93.0 (`cargo +1.93.0`). Keep one target directory, because the dependency tree is large. Run these checks before a pull request:

```sh
cargo +1.93.0 fmt --all -- --check
cargo +1.93.0 clippy --workspace --all-targets -- -D warnings
cargo +1.93.0 test --workspace
cargo +1.93.0 clippy -p polkameter-plugin-sdk --no-default-features --all-targets -- -D warnings
cargo +1.93.0 clippy -p polkameter-plugin-sdk --no-default-features --features chain --all-targets -- -D warnings
cargo +1.93.0 clippy -p polkameter-plugin-sdk --no-default-features --features observers --all-targets -- -D warnings
cargo +1.93.0 build -p polkameter --bin polkameter -p polkameter-example-plugin
tests/cli-smoke.sh
python3 tests/validate-plans.py
python3 tests/distribution.py
python3 tests/action-smoke.py
mdbook build
```

The workspace includes the desktop app, which needs GTK and WebKit libraries on Linux. To skip it, test the packages you changed with `-p`.

Tests that need a chain are `#[ignore]`d. Start the dev chain, run them, and stop it:

```sh
scripts/dev-chain.sh start
POLKAMETER_DEV_CHAIN_WS=ws://127.0.0.1:9944 cargo +1.93.0 test -p polkameter-substrate-plugin --test dev_chain -- --ignored
scripts/dev-chain.sh stop
```

`scripts/dev-chain.sh stop` stops only the node the script started. Don't kill other processes.

## Conventions

- Formatting follows `rustfmt.toml`: hard tabs, width 100. Lints in `Cargo.toml` deny warnings in CI.
- `polkameter-plugin-sdk` changes are additive: new items, no renamed or removed public items. Gate chain code behind the `chain` feature, and check each feature set alone with the clippy lines above.
- Plugins never log secrets. Errors about a secret URI name the input, never its value.
- Plans never hold secrets. They name credential profiles.
- New example plans go in `examples/` and must pass `polkameter validate` and the XSD.
- Keep plan templates identical to their example files. Keep CI templates identical to the jobs in `docs/dev-chain.md`. `tests/validate-plans.py` checks both.
- Docs follow `docs/STYLE.md`. Show real output from a real run, never invented output. Add new pages to `docs/SUMMARY.md` and `docs/llms.txt`.
- Pin GitHub Actions in new workflow steps to a full commit SHA, with the version in a comment.
- Commit messages are plain and describe the change. Don't add AI attribution: no `Co-Authored-By` lines for assistants and no "Generated with" footers, in commits or pull requests.
