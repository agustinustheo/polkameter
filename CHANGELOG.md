# Changelog

User-visible changes in each release. The GitHub release notes list every merged pull request.

## 0.3.0 (2026-11-08)

### Highlights

- The substrate plugin, `polkameter-substrate-plugin`, loads chains with the standard `System` and `Balances` pallets without plugin code. See [its limits](docs/substrate-plugin.md#limits). It funds signers derived from one seed, sends remark or transfer transactions and checks the chain's state after the run. The release archives, the install script, the container and the action include it. It isn't on crates.io.
- `polkameter init --rpc <endpoint>` writes a load plan for the substrate plugin from a live chain.
- `scripts/dev-chain.sh start|stop` runs a pinned, checksummed dev chain for local and CI load tests.
- You can install the CLI with `install.sh`, `cargo binstall`, the `ghcr.io/agustinustheo/polkameter` container or the release archives for Linux, macOS and Windows.
- Every run writes `junit.xml`, a test report that CI systems can display. `polkameter run --junit PATH` copies it.
- An Agent Skill, `AGENTS.md` and `docs/llms.txt` help AI coding assistants set PolkaMeter up. The repository is also a Claude Code plugin marketplace.

### Breaking changes

- The GitHub Action now fails the job on a `FAIL` or `INCONCLUSIVE` verdict by default (`fail-on: verdict-or-inconclusive`). Set `fail-on: error` for the old behavior.
- The action's artifact holds only the run's reports and records, not files that plugins write. Its default name is now unique per job, attempt and step. Read it from the `artifact-name` output, or set the `artifact-name` input.
- `polkameter run` exits `3`, not `1`, when it fails before setup, for example on a missing plugin or credential. Nothing was sent to the chain. Scripts that check for `1` must also accept `3`.
- `plugin list` prints the whole registry: `plugins`, `credentials`, `topologies` and `capabilities`. Read `.plugins` for the old output.
- With `--format json`, every line has `event` and `schemaVersion`, and a failed command prints an `error` line on standard output. See [Output formats](docs/cli.md#output-formats).
- Messages now start with `PolkaMeter:` and most plan, plugin and credential errors were reworded. Update scripts that match message text.
- The plugin SDK's crates are merged into `polkameter-plugin-sdk`, with the `chain` and `observers` features. `NodeSample`, `Loss` and `OnChain` are `#[non_exhaustive]`. See [Migrating from 0.2](docs/plugins.md#migrating-from-02).

### New

- `polkameter run --fail-on-inconclusive` exits `1` on an `INCONCLUSIVE` verdict. The reason line and `exit_reason` in `execution.json` say what failed the run.
- `agent serve` takes `--keep-runs`, and `--bind 127.0.0.1:0` picks a free port and prints it.
- Action inputs `artifact-retention-days` and `artifact-name`, and outputs `artifact-name`, `artifact-path`, `artifact-id` and `junit`. The `plugins` input accepts a bare executable name found on `PATH`.
- The SDK's `chain` feature reads runtime metadata and signs transactions for supported account types and transaction extensions, with `TxBuilder`. `keypair`, `derive_range` and `parse_account` make test signers from secret URIs.
- `--help` describes every command, argument and flag, with examples and the exit codes.

### Changed

- A dropped RPC connection is opened again and isn't counted as node down. A stalled chain still stops the load.
- A transaction's inclusion and latency count in the rate step that sent it.
- A flood sized exactly to the plan ends on the rate cap, not on budget used up.
- If the state check gives no result, the loss check can't pass and the verdict can't be `PASS`.
- Redaction now also covers load-test records, plugin `stderr.log` as it's written, endpoint URL secrets and JSON-escaped forms of a credential. See [Redaction](docs/results.md#redaction).
- Preflight checks every declared credential.
- `validate` checks built-in `core.` operations and their inputs. Plan and run errors name the file, line and column.
- Plans must be UTF-8. The [plan reference](docs/plans.md#validation-rules) gives the accepted XML declarations.

### Fixed

- `polkameter … | head -1` no longer panics.
- Plans from pipes, such as `<(envsubst < plan.tmpl)`, work again. Files over 2 MiB, directories and devices are refused with a clear message.
- Large plans parse much faster.
- Concurrent `plugin` commands no longer lose each other's changes.
