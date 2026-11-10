# AGENTS.md

Instructions for AI coding agents working on the PolkaMeter repository. To use PolkaMeter in another project, see the skill in [`skills/polkameter`](skills/polkameter/SKILL.md) instead.

## What this is

PolkaMeter is a load-testing tool for Polkadot SDK chains. The `polkameter` CLI runs XML plans. Plugins are separate executables that prepare chain-specific transactions. Plugins exchange one JSON message per line with the CLI through standard input and output.

## Layout

| Path | What it is | Published |
| --- | --- | --- |
| `crates/cli` | The `polkameter` CLI. Plan engine in `src/engine`, load runner in `src/load` | crates.io `polkameter` |
| `crates/plugin-sdk` | `polkameter-plugin-sdk`: protocol (default), `chain` feature (RPC, reads, signing, keys), `observers` feature (run files, checks) | crates.io `polkameter-plugin-sdk` |
| `plugins/substrate` | The substrate plugin, `polkameter-substrate-plugin`. Needs the standard `System` and `Balances` pallets and supported account and signature types | release archives only |
| `plugins/example` | Example plugin for tests. No chain | no |
| `examples/` | Example plans. `tests/validate-plans.py` validates every `*.polkameter.xml` here | |
| `schemas/` | The plan XSD | |
| `docs/` | mdBook user docs. Read `docs/STYLE.md` before you edit them | |
| `scripts/` | Release, packaging and dev-chain scripts | |
| `skills/polkameter` | Agent Skill for users who set up PolkaMeter. Its plan templates are copies of `examples/` | |
| `.claude-plugin` | Claude Code marketplace and plugin manifest. The plugin is the repository root | |

`polkameter-plugin-sdk` depends on no other PolkaMeter crate. `polkameter` depends on the SDK with every feature on. The engine and load modules are public only for tests. They are not a library API.

## How a run flows through the code

1. `engine::execute::run_stoppable` creates the run directory and writes events to `events.jsonl`.
2. `engine::plugins` starts one process per plugin. Each step sends an operation over the SDK protocol.
3. For a load test, `engine::measurement::run` starts the monitors (`engine/monitors`) and the load runner (`load`).
4. After the finality wait, `load::loss` gives each flood transaction its status.
5. `engine::checks::report::write_redacted` reads the files through the SDK's `observers`, runs the checks and writes `summary.json` and `summary.md`.
6. `engine::artifacts` writes `samples.jtl`. `execute` writes `execution.json`.

Each part writes its own raw files. Only the SDK's `observers` knows the formats. That's why `polkameter report` can run steps 5 and 6 again from the files alone.

Where to change things:

- New check: add its metric to `crates/plugin-sdk/src/observers/files/registry.rs`, record it in a monitor, and add the check under `crates/cli/src/engine/checks/outcomes/`.
- Stop rule or default threshold: `crates/cli/src/load/rules.rs`.
- Check limit: `crates/cli/src/engine/checks/limits.rs`.
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

Tests that need a chain are `#[ignore]`d, and `tests/rpc-outage.py` needs one too (with the substrate plugin installed and its `substrate-dev` credential profile, as in the CI dev-chain job). Start the dev chain, run them, and stop it:

```sh
scripts/dev-chain.sh start
POLKAMETER_DEV_CHAIN_WS=ws://127.0.0.1:9944 cargo +1.93.0 test -p polkameter-substrate-plugin --test dev_chain -- --ignored
tests/rpc-outage.py target/debug/polkameter target/dev-chain-runs/rpc-outage
scripts/dev-chain.sh stop
```

## Measurement integrity

PolkaMeter asserts only what its evidence shows, from the one place it watches the chain. So it never blames the chain for a fault of its own, of the network or of the RPC connection, and it never excuses the chain for a fault in it. [Whose fault is it](docs/reading-results.md#whose-fault-is-it) states the rules. These tests guard them. If a change makes one fail, fix the change, not the test. Tests without a file are in `crates/cli/tests/attribution.rs`.

| Rule | Test |
| --- | --- |
| A. An unknown submission is not failed inclusion; later replies or blocks resolve it | `unknown_submissions_are_not_failed_inclusions` |
| A. A disconnection shorter than 15 s is not node down, in the load and the loss check | `i1_a_node_back_within_the_reconnect_budget_is_not_down`, `the_loss_check_reconnects_within_the_budget`; `tests/rpc-outage.py` in the CI dev-chain job (an 8 s outage through a TCP proxy) |
| A. A connection that keeps dropping is unstable, not node down | `a_connection_that_keeps_dropping_is_unstable_not_node_down` |
| A. A restarted proxy is not a restarted node | `a_restarted_proxy_is_not_a_restarted_node` |
| A. A pause of PolkaMeter longer than about 1 s is not latency or reply time | `i3_time_polkameter_did_not_observe_never_makes_a_latency_failure`, `a_pause_of_any_length_never_makes_a_latency_failure`, `a_pause_is_never_the_nodes_reply_time`; `a_pause_leaves_out_every_time_measured_across_it` in `load_tracker.rs` |
| A. Block pace is the chain's own: no pause or delay on the way makes slow blocks | `a_pause_never_counts_in_block_gaps`, `a_delay_on_the_way_never_makes_slow_blocks`; `a_block_gap_is_the_chains_own_time` in `follower.rs` |
| A. A node or network that answers nothing for 15 s is not node down without evidence; a proxy is not the node's process | `a_node_or_network_unreachable_is_not_node_down`; `a_silent_connection_is_lost_from_the_last_answer` in `follower.rs`; `a_proxy_on_the_rpc_port_is_not_the_node` in `monitors/process.rs` |
| A. Reconnecting is not pool intake, and a resend has no latency | `i4_a_reconnecting_sender_is_never_pool_intake`, `i4_time_to_resend_after_a_reconnect_is_not_latency` |
| B. A node restart is node down in any phase | `a_restarted_node_is_node_down_in_any_phase`; `a_node_restart_is_node_down_in_load_and_recovery` in `load_runner.rs` |
| B. The reconnect budget is a hard limit; resuming a node keeps its stopped evidence | `a_reconnect_that_finishes_after_the_budget_is_too_late`, `a_resumed_node_does_not_erase_its_stopped_evidence` |
| B. An unreachable node whose process here ended, stopped or serves the RPC port is node down | `an_unreachable_node_whose_process_failed_is_node_down` |
| B. A finality stall fails the run in recovery and in the loss check, also when PolkaMeter paused | `a_finality_stall_in_recovery_fails_even_when_polkameter_paused`, `a_finality_stall_while_the_loss_check_runs_fails_the_run` |
| C. A baseline or recovery PolkaMeter didn't observe has no result, and no drain rate | `i3_a_pause_in_the_baseline_or_recovery_is_not_the_chains`, `a_baseline_polkameter_did_not_observe_has_no_result`, `a_recovery_polkameter_did_not_observe_is_not_measured` |
| C. A baseline with no time makes the verdict at most INCONCLUSIVE | `a_baseline_with_no_time_is_at_most_inconclusive` in `engine/checks/tests.rs` |
| C. Inclusions seen before recovery are not its drain | `inclusions_seen_before_recovery_are_not_its_drain` |
| C. A transaction without a measured time has no time in any export | `untimed_transactions_have_no_time_in_any_export` |
| C/D. Absence from READY is not loss; a finalized nonce past an absent transaction proves loss | `an_accepted_transaction_absent_from_ready_may_still_finalize`, `loss_requires_a_consumed_finalized_nonce` in `crates/cli/src/load/loss.rs`; `unresolved_transactions_are_inconclusive_but_proven_loss_fails` in `engine/checks/tests.rs` |
| D. Every unknown transaction has a reason | `every_unknown_transaction_has_a_reason` in `crates/cli/src/load/loss.rs` |
| D. Loss statuses partition what was sent | `statuses_partition_sent_in_every_scenario` in `crates/cli/src/load/loss.rs` |
| D. The loss check walks from below every flood transaction | `the_loss_check_walks_from_below_every_flood_transaction` |
| Diagnostics name what PolkaMeter saw | `a_node_down_in_the_baseline_is_named`, `a_pause_is_not_called_a_reconnect`, `heads_without_a_block_read_are_not_no_new_block`; `blocks_unobserved_for_longer_than_a_stall_are_no_result` in `engine/checks/tests.rs` |

## Conventions

- Formatting follows `rustfmt.toml`: hard tabs, width 100. Lints in `Cargo.toml` deny warnings in CI.
- `polkameter-plugin-sdk` changes are additive: new items, no renamed or removed public items. Gate chain code behind the `chain` feature, and check each feature set alone with the clippy lines above.
- Plugins never log secrets. Errors about a secret URI name the input, never its value.
- Plans never hold secrets. They name credential profiles.
- New example plans go in `examples/` and must pass `polkameter validate` and the XSD.
- Keep plan templates identical to their example files. Keep CI templates identical to the jobs in `docs/ci.md`. `tests/validate-plans.py` checks both.
- Docs follow `docs/STYLE.md`. Add new pages to `docs/SUMMARY.md` and `docs/llms.txt`.
- Pin GitHub Actions in new workflow steps to a full commit SHA, with the version in a comment.
- Commit messages are plain and describe the change. Don't add AI attribution: no `Co-Authored-By` lines for assistants and no "Generated with" footers, in commits or pull requests.
