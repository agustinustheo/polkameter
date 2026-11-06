# AGENTS.md

Instructions for AI coding agents working on the PolkaMeter repository. For using PolkaMeter in another project, see the skill in [`skills/polkameter`](skills/polkameter/SKILL.md) instead.

## What this is

PolkaMeter is a load-testing tool for Polkadot SDK chains. The `polkameter` CLI runs XML plans. Plugins are separate executables that prepare chain-specific transactions and talk to the CLI over a JSON-line protocol on standard input and output.

## Layout

| Path | What it is | Published |
| --- | --- | --- |
| `crates/cli` | The `polkameter` CLI: plan engine (`src/engine`), load runner (`src/load`), remote agent, `init` | crates.io `polkameter` |
| `crates/plugin-sdk` | `polkameter-plugin-sdk`: protocol (default), `chain` feature (RPC, reads, signing, keys), `observers` feature (run files, checks) | crates.io `polkameter-plugin-sdk` |
| `plugins/substrate` | The substrate plugin, `polkameter-substrate-plugin`, for any chain with `System` and `Balances` | release archives and container only |
| `plugins/example` | Example plugin for tests; no chain | no |
| `src-tauri`, `src` | Desktop app (Tauri, TypeScript). Not documented for users | no |
| `examples/` | Example plans. `tests/validate-plans.py` validates every `*.polkameter.xml` here | |
| `schemas/` | The plan XSD | |
| `docs/` | mdBook user docs. Read `docs/STYLE.md` before editing | |
| `scripts/` | Release, packaging, action and dev-chain scripts | |
| `skills/polkameter` | Agent Skill for users setting up PolkaMeter. Its plan templates are copies of `examples/` | |
| `.claude-plugin` | Claude Code marketplace and plugin manifest; the plugin is the repository root, whose `skills/` holds the skill | |

## Build and test

Use Rust 1.93.0 (`cargo +1.93.0`). Keep one target directory; the dependency tree is large.

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

`scripts/dev-chain.sh stop` stops only the node the script started. Do not kill other processes.

## Conventions

- Formatting follows `rustfmt.toml`: hard tabs, width 100. Lints in `Cargo.toml` deny warnings in CI.
- `polkameter-plugin-sdk` changes are additive: new items, no renamed or removed public items. Gate chain code behind the `chain` feature, and check each feature set alone with the clippy lines above.
- Plugins never log secrets. Errors about a secret URI name the input, never its value.
- Plans never hold secrets; they name credential profiles.
- New example plans go in `examples/` and must pass `polkameter validate` and the XSD. A plan template in `skills/polkameter/templates` must equal its `examples/` file, and the CI templates must equal the jobs in `docs/dev-chain.md`; `tests/validate-plans.py` checks both.
- Docs follow `docs/STYLE.md`: show real output from a real run, never invented output. Add new pages to `docs/SUMMARY.md` and `docs/llms.txt`.
- Pin GitHub Actions in new workflow steps to a full commit SHA, with the version in a comment.
- Commit messages are plain and describe the change. Do not add AI attribution: no `Co-Authored-By` lines for assistants and no "Generated with" footers, in commits or pull requests.
