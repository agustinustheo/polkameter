# XML plans and Rust plugins

Polkameter uses JMeter's separation between declarative plans and executable components. It remains a Rust engine; the plan is a Polkameter XML schema, not arbitrary JMeter JMX or a Java plugin interface.

The core works with any Polkadot SDK chain: it submits prepared extrinsics at a controlled rate, follows best and finalized blocks, reconciles every transaction, scrapes node metrics and, for a parachain, observes the relay. Anything specific to one runtime (how to build its transactions, what state they should leave, which pallets to watch) belongs in a plugin.

## Build and try it

From the repository root, using Rust 1.93 or newer. The CLI shares a crate with the desktop app, so build the frontend first:

```sh
pnpm install --frozen-lockfile
pnpm build
cargo build --workspace --bins
export POLKAMETER_PLUGIN_REGISTRY="$PWD/target/plugins.json"
target/debug/polkameter plugin install target/debug/polkameter-example-plugin
target/debug/polkameter validate examples/plugin-workflow.polkameter.xml
target/debug/polkameter plugin inspect examples/plugin-workflow.polkameter.xml
target/debug/polkameter run examples/plugin-workflow.polkameter.xml --output target/runs
```

The example executable is a separate Cargo package depending on `polkameter-plugin-sdk`. A plugin can live in another repository and be installed without rebuilding the host. `plugin install` reads its manifest and pins its version and executable BLAKE2 hash. Reinstall after rebuilding an executable. XML cannot select an arbitrary executable path.

The registry defaults to `~/.config/polkameter/plugins.json`; `POLKAMETER_PLUGIN_REGISTRY` overrides it. On Windows, `USERPROFILE` is used if `HOME` is absent. `plugin credential PROFILE ENV_VAR` maps a credential profile to an environment variable, and `plugin topology ALIAS zombie.json` registers a network topology; plans name the profile and the alias, never the secret or the path.

## Plan execution

[The XSD](../schemas/polkameter-plan.xsd) defines structural authoring rules; host validation additionally resolves references, schemas, limits and installed versions. Processing order is:

1. Resolve installations and credentials; validate every operation contract.
2. Check required evidence, run read-only preflight steps and, for a load, calibrate the block interval.
3. Run setup once.
4. Run each workflow with bounded users, iterations and concurrency.
5. If configured, run baseline, rate steps, recovery, finality wait and reconciliation.
6. Run evaluate steps.
7. Write the report: built-in checks, then the checks plugins returned in evaluate steps.
8. Attempt every teardown step, shut down plugins and persist the outcome.

`steps.ID.OUTPUT` addresses a declared output; `targets.ID` and `credentials.ID` address configuration. `run.id`, `run.probeBudget` and `run.directory` are host values. Workflows also have `user.index` and `iteration.index`. Setup outputs are copied into each user/iteration; workflow outputs cannot leak into another user, iteration or workflow. Forward references and unknown output names fail before setup.

An `<input>` has exactly one `ref` or `value`. Literals parse as JSON when possible and otherwise as strings. Use XML escaping for JSON strings and structured literals. No code is evaluated from XML.

`core.submit-prepared` waits for an RPC reply. `core.submit-setup` additionally waits for finalized inclusion and checks dispatch/Sudo results, returning transaction hashes and the finalized block hash. A timeout after sending is ambiguous and is never automatically retried.

## Measured load

`<load target="..." source-ref="..." probes-ref="...">` submits transactions a plugin prepared: two references to lists of `PreparedTx` (inline or as an `Artifact`), one for the load and one reserved for baseline and recovery probes. They must be disjoint, and each hash is sent at most once across connections. `run.probeBudget` tells the preparing step how many probes the run needs.

`<rate tx-per-second="4" seconds="30"/>` defines sustained arrivals; alternatively `<ramp start="2" step="2" steps="3" seconds="30"/>` or `growth="2"`. Exhausting the prepared budget or saturating the generator is an explicit stop reason. `block-interval-seconds` states the block time the plan expects; preflight measures the timestamp span of recent blocks, keeps both values in `calibration.json`, and stops before setup if they differ by more than 25%.

`<thresholds>` overrides the default stop rules: `stall-ms`, `finality-stall-ms`, `max-p95-latency-ms`, `max-submit-reply-ms`, inclusion/refusal/send ratios, block-gap factors, recovery probe count and finality wait. Values and rules are retained in the report.

After recovery the run snapshots the ready pool, waits until the chain has finalized past the best block at that moment, and walks the finalized chain. Each submitted transaction gets exactly one status in `transactions.jsonl`: `finalized`, `in_pool`, `refused`, `expired`, `lost` or `unknown`. Missing pool or finality evidence makes a transaction `unknown`, never `lost`. With `state-check` and `state-ref`, a plugin operation receives `target`, `state`, `hashes` and `at` for a sample of finalized transactions and returns `checked`, `missing` and `detail`.

## Monitors

`<monitors topology="ALIAS" relay-target="TARGET" para-id="N">` scrapes every node of the registered Zombienet topology: the collators of parachain `N` (role `collator`), the relay node inside each collator (`collator-relay`) and the relay validators (`validator`). The relay recorder follows the relay's finalized blocks for backing, inclusion, timeouts, disputes and the slots offered to each parachain; the relay checks judge parachain `N`. Child `<metric role="collator" name="substrate_block_height"/>` entries make a metric family required on every node of that role. The process sampler records CPU and memory of the node under load (`POLKAMETER_NODE_PID`, else the process listening on its RPC port).

`plugin inspect PLAN` is offline with respect to the chain: it validates installed manifests and exports target declarations, monitor requirements and the node metric catalog. A provisioner should consume this before starting a network. Required manifest evidence supports `rpc` (a target and an RPC method checked against `rpc_methods`), `metric` (a role and a metric family) and `capability` (registered by the provisioner with `plugin capability NAME`). `scripts/verify-previewnet-requirements.py` checks the exported requirements against a bitten Previewnet bundle before the network starts.

## Plugin observers and checks

A plugin can watch the chain during a run and judge what it saw:

- **Observer.** A setup step starts it and an evaluate step stops it. It writes `series.jsonl` (the same records as `chain.jsonl`, e.g. with `polkameter-monitors`' `ChainSeries` and `walker`) and `metrics.json` (a list of `PluginMetric`: `name`, `kind`, `help`, `buckets`) into its plugin directory. The checks read these series with the run's own, labelled `job="plugin"` and `instance=<plugin id>`. A plugin may not redefine a built-in metric.
- **Check.** An evaluate step whose operation declares a `checks` output contributes check results: a list of `{outcome, check, status, detail, numbers?, optional?}`, with `status` one of `pass`, `warn`, `fail`, `info`, `no result`. Pass `run.directory` to it; `polkameter_files::read_store` and `summary.json` give it a `polkameter_checks::RunData`, the same data the built-in checks use. Smoke mode fails on a check without a result unless it is `optional`.

The report keeps plugin results in `plugin-checks.json`, so `polkameter report RUN_DIRECTORY` reproduces the same summary offline.

## Plugin protocol 1

One executable process lives for the run. Calls to each plugin are serialized; different plugins and workflows can run concurrently. Long preparation runs should write batches to artifacts. Plugins may retain state keyed by run, user and iteration. Process separation is a compatibility and lifecycle boundary; installed plugins are trusted code, not sandboxed.

The SDK's `Plugin` trait supplies a manifest and async `invoke`. `serve` implements newline-delimited JSON on stdin/stdout. Standard output is reserved for protocol replies. Run-time plugin stderr is retained in `plugins/<id>/stderr.log` (owner-only permissions on Unix); plugins must not write credentials to it. Phase, invocation duration and periodic progress events come from the host.

```json
{"protocol":1,"id":1,"operation":"double","inputs":{"value":21},"context":{"run_id":"run-1","artifact_dir":"/run/plugins/example","user":null,"iteration":null}}
{"protocol":1,"id":1,"result":{"value":42},"error":null}
```

`$describe` returns ID, exact version, protocol, operation schemas and environment requirements. `$shutdown` acknowledges bounded shutdown. Each reply must match its request ID and contain exactly one result or error. The SDK supports string, integer, number, boolean, array, object and arbitrary JSON schemas. Both host and SDK check input/output shapes. A message is capped at 8 MiB, including streams without a newline.

Large outputs use SDK `Artifact` references: version, file path and BLAKE2 hash. The host requires referenced files to remain inside the run directory and caps reads at 256 MiB. `PreparedTx` validates bytes against their declared hash. Name artifacts uniquely when an operation runs more than once.

Deadlines include time waiting for the plugin's serialization lock. An operation error (including a failed assertion), crash, malformed reply, cancellation or timeout terminates the process; it is not restarted mid-run. The host does not assume plugin state is reusable after an error and never retries the operation. Remaining teardown operations are attempted, but a terminated plugin cannot perform its own cleanup. Whole-run and teardown deadlines bound shutdown. Secret input values are excluded from host events and redacted from errors.

## Results, desktop and remote workers

A run directory includes the normalized plan, installation manifests and checksums, `events.jsonl`, `execution.json` and JMeter-compatible `samples.jtl`. Load plans additionally retain raw node scrapes, the load and chain series, blocks, steps, `transactions.jsonl`, `summary.json` and `summary.md`. Run `polkameter report RUN_DIRECTORY` to regenerate the checks and SVG plots offline, also after copying a bundle.

Stress mode measures degradation and may return zero despite failed chain-health checks. Smoke mode additionally fails on monitor problems and required checks without results. Tool or setup failures and failed explicit XML assertions are nonzero in both modes.

The desktop app opens and saves plans, derives input editors from installed manifests and uses the same engine for preflight, run and stop. Desktop run bundles go to the operating system's Polkameter application-data directory under `runs/`. Remote agents expose authenticated `/plugins`, `/inspect`, `/preflight` and `/runs` routes, with run-specific status and stop routes. Install plugins and configure credential and topology profiles on the worker; the client sends XML, not executable paths or secret values. The remote bearer token grants full execution trust, including any credential profile and target endpoint configured on that agent; use separate agents for separate trust domains.

```sh
# Worker, using its own registry and token environment:
polkameter agent serve --bind 127.0.0.1:9901 --output-root /data/runs
# Client, over a loopback tunnel or HTTPS:
polkameter run scenario.xml --remote http://127.0.0.1:9901 --remote-token-env AGENT_TOKEN
```

## Running on a local network

`scripts/local-fork-run.sh NETWORK_TOML PLAN [OUTPUT_DIR]` spawns a Zombienet network (for example a Previewnet fork from `ppn fork toml <bundle> <out>`) into a new directory, waits until every WebSocket target of the plan produces and finalizes blocks, installs `POLKAMETER_PLUGINS`, maps `POLKAMETER_CREDENTIALS` (`profile=ENV_VAR`), registers the plan's topology, runs the plan and stops the network.

## Development checks

```sh
cargo test --workspace
cargo fmt --all -- --check
cargo clippy --workspace --exclude polkameter --all-targets -- -D warnings
pnpm test
pnpm build
```

`scripts/plugin-benchmark.py PATH_TO_EXAMPLE_PLUGIN` measures JSONL call overhead.

The load, monitor, check and file crates derive from the stress tool in [paritytech/polkadot-pop-e2e](https://github.com/paritytech/polkadot-pop-e2e), under Apache-2.0.
