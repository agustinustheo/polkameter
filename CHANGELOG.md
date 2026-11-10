# Changelog

User-visible changes in each release. The GitHub release notes list every merged pull request.

## 0.4.3 (2026-11-10)

### Fixed

- The loss check no longer calls finalized transactions lost when a fork below the first block PolkaMeter read finalized them. It walks the finalized chain from the block above the one finalized before the first flood transaction.
- Slow blocks and the recovery's block pace use the chain's own timestamps. A proxy that delayed blocks by 2 s made a 13 s rate step "slow blocks" on a chain that was on time.
- A node or network that answers nothing for 15 s, on a closed or an open connection, now ends the load on `generator limit: node or network unreachable` (INCONCLUSIVE). PolkaMeter watches from one place and can't tell the two apart. It's node down only when the node came back restarted, or its process on this machine ended, is stopped, or serves the RPC port itself. A healthy node behind a proxy cut for 15.5 s was node down, and a node frozen for 20 s with its socket open passed.
- Node CPU and memory come only from a process that also listens on one of the node's p2p ports. A proxy on the RPC port was sampled as the node.
- `samples.jtl` has no row for a transaction without a measured time. It wrote 0 ms for one PolkaMeter couldn't time, and the 1 ms reply time for one it saw only on the finalized chain.
- A recovery that isn't measured has no drain rate (`drainPerS` is `null`). The drain counts only blocks announced in recovery, not blocks read late after a reconnect.
- A baseline with no timed probe makes the verdict at most INCONCLUSIVE. It passed.
- A node restart in the baseline is named in the run's error. Warnings say whether PolkaMeter paused or reconnected. A recovery whose new blocks were announced but not read says so, not "no new block".

### Changed

- Plugin SDK: `Recovery::drain_per_s` is `Option<f64>`, `None` when the recovery isn't measured. `BlockRecord::gap_ms` is the chain's own gap from block timestamps, not the time between arrivals. Summaries from earlier versions still read.
- [Whose fault is it](docs/reading-results.md#whose-fault-is-it) states what PolkaMeter can't see from one place. Latency and reply times include the network between PolkaMeter and the node. Pauses of PolkaMeter shorter than about 1 s go unseen and can add up to that much to a time.

## 0.4.2 (2026-11-10)

### Fixed

- A pause of PolkaMeter itself shorter than 5 s no longer counts as the chain's latency, submit reply time or block gap. Any stall of PolkaMeter's own loop of more than about 1 s now leaves out every time measured across it. A 4.9 s pause made a latency failure onset, a "pool intake" stop and slow blocks.
- A baseline whose probes PolkaMeter timed none of now has no result, with a warning, instead of `0 ms`. `summary.json` writes `p50Ms` and `maxMs` as `null`. A recovery that is not back is then `not measured`, because its threshold can't come from the baseline.
- A transaction PolkaMeter couldn't time has no time in the exports either: `inclusionMs` is `null` and `untimed` is `true` in `transactions.jsonl`, the inclusion histogram leaves it out, and `samples.jtl` labels it `load:step:N:untimed`. A rate step with no timed transaction shows `-` for p50 and p95, not `0.0`.
- A finality stall or a stall while the loss check waits for finality now fails the run, as in the load. The block follower keeps following the chain until the loss check ends. A 90 s finality stall there passed.
- Recovery now ends on a stall, a finality stall or node down, and fails, also when PolkaMeter paused in it. A recovery that PolkaMeter didn't fully observe before the chain was back (a pause, a dropped connection, an untimed probe) is `not measured`, never a failed recovery.
- The loss check opens its connection again, with the 15 s budget, whenever a read fails. A disconnect during the loss check left transactions unverified.
- A node restart is now node down in every phase. PolkaMeter detects it from the node's identity after a reconnect: the same peer ID with a new sync starting block. It was a warning, and the load ended on "generator limit".
- A restarted proxy in front of the node is no longer node down. A new process on the endpoint's port is only a monitoring problem now.
- A connection that drops more than 5 times in 60 s, with the node not restarted, now ends the load on "generator limit" with `connection unstable` in the detail, not on "node down". PolkaMeter keeps reconnecting.
- Stop details say what PolkaMeter knows. Send rates show two decimals and the 95% rule (`sent 1.89 tx/s, 94.5% of the 2 tx/s target (95.0% needed)`), not `sent 2 of 2 tx/s`. "Pool intake" names the node or the network between. The loss check no longer says `blocks 0-0` when it couldn't walk the chain. The untimed-transactions warning no longer says their time isn't the chain's.
- A run that exits `1` only because its verdict is INCONCLUSIVE now ends in the state `completed_inconclusive`, not `completed_with_failures`.

### Changed

- Plugin SDK: `Baseline::p50_ms` and `Baseline::max_ms` are `Option<Millis>`: `None` when PolkaMeter timed no probe. `summary.json` files from earlier versions still read.

## 0.4.1 (2026-11-10)

### Fixed

- A dropped RPC connection gets the full 15 s to reconnect before the node counts as down. It gave up after 7.75 s, so an 8 s outage stopped the load on "node down". The loss check reconnects with the same budget, so it still verifies the loss after an outage.
- A pause of PolkaMeter itself, blocks read late after a reconnect, or a transaction sent again after its connection dropped no longer make a latency failure onset. Their transactions are left out of p50 and p95, and a warning gives the count. `steps.jsonl` counts them per rate step in `untimed`.
- A pause of PolkaMeter in the baseline no longer counts in the baseline probe times. A pause in recovery makes the recovery `not measured`, instead of a slow recovery.
- Time when every RPC connection is down no longer stops the load on "pool intake". That rule now needs bytes unsent on an open connection. A rate step that sent too little while reconnecting ends on "generator limit".

## 0.4.0 (2026-11-09)

### Removed

- The remote agent: `agent serve` and `polkameter run --remote`. Run the CLI on that host instead, for example `ssh host polkameter run plan.polkameter.xml --output runs`.
- The desktop app.
- The Windows release archive. On Windows, install with `cargo install --locked polkameter`.
- The `ghcr.io/agustinustheo/polkameter` container image. Install the CLI with `install.sh` in any Linux image, as the GitLab CI template does.
- The `polkameter.spdx.json` release asset. The build attestations still cover every archive.
- `scripts/local-fork-run.sh`. Start the Zombienet network yourself, register its `zombie.json` with `polkameter plugin topology`, then run the plan.
- `<workflow>`. Use `<setup>` steps, or `<load>` for load.
- `--format json`. Read `execution.json`, `summary.json` and `events.jsonl` in the run directory. `plugin inspect` and `plugin list` still print JSON.
- `polkameter run --junit PATH`. Read `run-*/junit.xml`.
- `polkameter run --fail-on-verdict` and `--fail-on-inconclusive`. Use `--fail-on verdict` or `--fail-on verdict-or-inconclusive`.
- `plots/*.svg`. JMeter can chart `samples.jtl`.
- Plugin `requirements` and `polkameter plugin capability`. `<monitors>` already checks the metrics a plan needs.
- The action's `install`, `toolchain`, `plugin-crates`, `plugin-unlocked`, `topologies` and `capabilities` inputs, and building the action from a branch. Build plugins with cargo in an earlier step and pass them in `plugins`. For topologies, set `POLKAMETER_PLUGIN_REGISTRY` and run `polkameter plugin topology` before the action. A branch or SHA ref needs `version`.

### Changed

- `polkameter run` exits `1` on a `FAIL` or `INCONCLUSIVE` load verdict by default, like the action. `--fail-on error|verdict|verdict-or-inconclusive` sets the policy, the same values as the action's `fail-on` input, which passes it through, so the action's `version` input must be 0.4.0 or later. Set `--fail-on error` for the old behavior. Smoke mode still fails on monitor problems and required checks with no result under every policy.
- The action installs PolkaMeter from the release with `install.sh`, which checks the checksum and the build attestation, and uploads the run with `actions/upload-artifact`.
- Preflight starts the plan's plugins once, not twice.
- Plugin SDK: the unused chain API (`chain::derive`, `chain::scale_value_reexport`, `TxBuilder::at`, `TxBuilder::has_constant`, `Client::spendable`) and `Response::into_result` are removed. The SDK no longer depends on `scale-value`, `scale-decode`, `frame-decode`, `futures-util` and `subxt-rpcs`; use subxt's re-exports.
- Plugin SDK: the CLI's built-in checks, summary report and its own chain reads move into the CLI (`Check`, `run`, `smoke_gaps`, `report`, `registry::Outcome`, `FinalStep`, `Client::{normal_limit, block_interval_s, parent, pending}`). The `chain::{client, keys, reads, sign, tx}` and `observers::{checks, files, monitors}` modules are private. Use the flat `chain::*` and `observers::*` paths, as in [Migrating from 0.2](docs/plugins.md#migrating-from-02).
- Plugins built with SDK 0.3 keep working. PolkaMeter accepts their `requirements`, and existing registry files still load.

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
- With `--format json`, every line has `event` and `schemaVersion`, and a failed command prints an `error` line on standard output.
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
