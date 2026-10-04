# XML plans and Rust plugins

Polkameter uses JMeter's separation between declarative plans and executable components. It remains a Rust engine; XML v2 is a Polkameter schema, not arbitrary JMeter JMX or a Java plugin interface. Existing v1 plans continue to use the signed-call runner.

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

The registry defaults to `~/.config/polkameter/plugins.json`; `POLKAMETER_PLUGIN_REGISTRY` overrides it. On Windows, `USERPROFILE` is used if `HOME` is absent.

## Run the migrated People workload

Use a disposable PreviewNet fork with compatible People metadata and RPC access. The plugin checks transaction-extension order and claim call encoding before setup. Recognition uses sudo; claims use ring proofs, not an sr25519 signature from the setup account.

```sh
target/debug/polkameter plugin install target/debug/polkameter-people-plugin
target/debug/polkameter plugin credential previewnet-sudo POLKAMETER_SETUP_SURI
target/debug/polkameter plugin topology previewnet /absolute/path/to/data-fork/zombie.json
export POLKAMETER_SETUP_SURI=//Alice  # development authority on the disposable fork

target/debug/polkameter preflight examples/people-one-claim.polkameter.xml
target/debug/polkameter run examples/people-one-claim.polkameter.xml --output target/runs
target/debug/polkameter run examples/people-smoke.polkameter.xml --output target/runs
```

Endpoints are XML values. Credential values are resolved on the executing host from the named environment-variable profile; no SURI belongs in the XML. `--signer-env NAME` overrides the environment mapping when a plan declares exactly one credential.

| Plan | Members × slots | Offered load | Recovery / baseline |
| --- | --- | --- | --- |
| `people-one-claim` | 2 × 1 | One claim; one unused reserve | Explicit finalized-state assertion |
| `people-smoke` | 40 × 12 | 2, 4, 6 tx/s; 30 seconds each | 120 s / 3 probes |
| `people-capacity` | 250 × 20 | 12, 15, 18, 21 tx/s; 60 seconds each | 900 s / 5 probes |
| `people-default` | 750 × 20 | 6 + 4 per step; ten 60-second steps | 900 s / 5 probes |

Capacity and default expect the PR's **3 People cores and 5 collators**, with a 2-second expected interval; default is the upstream workload when no arguments are given. Smoke expects 1 core and 1 collator, with a 6-second interval. These are explicit scenario parameters: confirm them against the attached network. Core/collator allocation is captured when biting the fork; adding five processes to a one-collator snapshot is not equivalent.

## Plan execution

[The XSD](../schemas/polkameter-plan-v2.xsd) defines structural authoring rules; host validation additionally resolves references, schemas, limits and installed versions. Processing order is:

1. Resolve installations and credentials; validate every operation contract.
2. Check required evidence and run read-only preflight steps.
3. Run setup once.
4. Run each workflow with bounded users, iterations and concurrency.
5. If configured, run baseline, rate steps, recovery, finality wait and reconciliation.
6. Run evaluation steps.
7. Attempt every teardown step, shut down plugins and persist the outcome.

`steps.ID.OUTPUT` addresses a declared output; `targets.ID` and `credentials.ID` address configuration. `run.id` and `run.probeBudget` are host values. Workflows also have `user.index` and `iteration.index`. Setup outputs are copied into each user/iteration; workflow outputs cannot leak into another user, iteration or workflow. Forward references and unknown output names fail before setup.

An `<input>` has exactly one `ref` or `value`. Literals parse as JSON when possible and otherwise as strings. Use XML escaping for JSON strings and structured literals. No code is evaluated from XML.

`<rate tx-per-second="4" seconds="30"/>` defines sustained transaction arrivals. Alternatively use `<ramp start="2" step="2" steps="3" seconds="30"/>` or `growth="2"`. Each prepared hash is allocated once across connections. Flood and reserved probe artifacts must be disjoint. Proof generation finishes before measured traffic starts; exhausting the prepared budget or saturating the generator is an explicit stop reason.

`<thresholds>` overrides PR defaults, including `stall-ms`, `finality-stall-ms`, `max-p95-latency-ms`, `max-submit-reply-ms`, inclusion/refusal/send ratios, block-gap factors, recovery probe count, and finality wait. Values and rules are retained in the report.

`core.submit-prepared` waits for an RPC reply. `core.submit-setup` additionally waits for finalized inclusion and checks dispatch/Sudo results, returning transaction hashes and the finalized block hash. A timeout after sending is ambiguous and is never automatically retried.

## Plugin protocol 1

One executable process lives for the run. Calls to each plugin are serialized; different plugins and workflows can run concurrently. Long preparation runs should batch proofs into artifacts. Plugins may retain state keyed by run/user/iteration, and may start a background observer in a setup operation and stop it in evaluation/teardown. Process separation is a compatibility and lifecycle boundary; installed plugins are trusted code, not sandboxed.

The SDK's `Plugin` trait supplies a manifest and async `invoke`. `serve` implements newline-delimited JSON on stdin/stdout. Standard output is reserved for protocol replies. Run-time plugin stderr is retained in `plugins/<id>/stderr.log` (owner-only permissions on Unix). Plugins must avoid writing credentials to stderr; these raw trusted-plugin diagnostics are not automatically redacted. Phase, invocation duration and periodic progress events come from the host. Plugin-specific progress messages are not part of protocol 1.

```json
{"protocol":1,"id":1,"operation":"double","inputs":{"value":21},"context":{"run_id":"run-1","artifact_dir":"/run/plugins/example","user":null,"iteration":null}}
{"protocol":1,"id":1,"result":{"value":42},"error":null}
```

`$describe` returns ID, exact version, protocol, operation schemas and environment requirements. `$shutdown` acknowledges bounded shutdown. Each reply must match its request ID and contain exactly one result or error. The SDK supports string, integer, number, boolean, array, object and arbitrary JSON schemas. Both host and SDK check input/output shapes. A message is capped at 8 MiB, including streams without a newline.

Large outputs use SDK `Artifact` references: version, file path and BLAKE2 hash. The host requires referenced files to remain inside the run directory and caps reads at 256 MiB. `PreparedTx` validates bytes against their declared hash. Name artifacts uniquely when an operation runs more than once (for example include user and iteration). Original preparation references carry local paths; portable report replay uses the retained ledger/raw observations, not those original filesystem locations.

Deadlines include time waiting for the plugin's serialization lock. An operation error (including a failed assertion), crash, malformed reply, cancellation or timeout terminates the process; it is not restarted mid-run. This is a conservative fail-stop policy: the host does not assume plugin state is reusable after an error or retry the operation. Remaining teardown operations are attempted, but a terminated plugin cannot perform its own cleanup. Whole-run and teardown deadlines bound shutdown. Preparation artifacts can contain publicly submitted transaction bytes and public claim metadata; secret input values are excluded from host events and redacted from errors.

## Evidence and provisioning

`plugin inspect PLAN` is offline with respect to the chain: it validates installed manifests and exports target declarations, monitor requirements and the PR metric catalog. A provisioner should consume this **before** starting a network. Required manifest evidence supports:

- `rpc`: target alias plus an RPC method verified against `rpc_methods`;
- `metric`: node role plus metric family required on every node of that role;
- `capability`: an instrumentation capability explicitly registered by the provisioner with `plugin capability NAME`.

`scripts/verify-previewnet-requirements.py` checks the exported requirements against the bitten bundle before the network starts, and `ppn fork wait` requires best and finalized progress on every chain before the run. The Previewnet adapter uses the HTTP RPC endpoint corresponding to each declared WebSocket endpoint. Unsupported required kinds fail preflight. A capability declaration is an attestation by the provisioner, not automatic proof that a trace parser is working.

The migrated monitor package discovers validators, People collators and their embedded relay nodes from Zombienet. `<monitors>` selects that package; it is not automatically applied to every generic plugin workflow. Child `<metric role="people-collator" name="substrate_block_height"/>` entries enforce required families. Other catalog metrics can be absent before their first event; warnings are preserved and offline checks determine whether sufficient evidence was recorded. Scrape failures, missing observations and counter resets remain distinct from successful health verdicts.

The People/relay/Recycler recorders are presently a bundled Polkadot monitor package. Runtime-specific preparation and state checks are external plugin operations. Moving the bundled recorders into separately installed observer executables is a subsequent packaging step, not a prerequisite for adding a custom observer through setup/evaluation operations.

## Results, desktop and remote workers

A v2 bundle includes the normalized plan, installation manifests/checksums, `events.jsonl`, `execution.json`, and JMeter-compatible `samples.jtl`. Load plans additionally retain raw scrapes, blocks, steps, OpenMetrics, reconciliation records and structured/Markdown reports. `transactions.jsonl` separates acceptance replies, best-chain observations, finalized inclusion, refusal, pool presence, confirmed accounting gaps and unknown outcomes. Missing pool/finality evidence cannot prove loss. The state check samples up to 100 finalized successful claims; it does not assert every claim's unique target, and retains PR 37's target mapping for comparison.

Run `polkameter report RUN_DIRECTORY` to regenerate built-in checks and SVG plots offline. This works after copying a bundle. Numeric plots use the JTL samples; v2 raw node/scrape files remain authoritative for monitor evidence. The legacy telemetry plots are empty where no v1 telemetry was collected. A JTL sample marked `finalized` is successful only when finalized dispatch also succeeded. Step timing and load timing have distinct labels.

Stress mode measures degradation and may return zero despite failed chain-health checks. Smoke mode additionally fails on observation gaps and required checks without results. Tool/setup failures and failed explicit XML assertions are nonzero in both modes. Existing v1 exit semantics are unchanged.

The desktop's **Plugin plans** workbench opens/saves XML, derives input editors from installed manifests, and uses the same engine for preflight/run/stop. Remote workers expose authenticated `/v2/plugins`, `/v2/inspect`, `/v2/preflight`, `/v2/runs`, and run-specific status/stop routes. Install plugins and configure credential/topology profiles on the worker; the client sends XML, not executable paths or secret values. The agent retains the last 32 statuses in memory and durable artifacts on disk.

```sh
# Worker, using its own registry and token environment:
polkameter agent serve --bind 127.0.0.1:9901 --output-root /data/runs
# Client, over a loopback tunnel or HTTPS:
polkameter run scenario.xml --remote http://127.0.0.1:9901 --remote-token-env AGENT_TOKEN
```

## Development checks

```sh
cargo test --workspace
cargo fmt --all -- --check
cargo clippy --workspace --exclude polkameter --all-targets -- -D warnings
python3 scripts/check-proof-features.py
pnpm test
pnpm build
```

`verifiable` stays pinned at 0.3.0 without `ark-vrf/parallel`: nested proof pools were an upstream failure mode. `scripts/plugin-benchmark.py PATH_TO_EXAMPLE_PLUGIN` measures JSONL call overhead independently from proof generation and chain load. Source attribution and the reviewed PR revision are in [pr37-source.md](pr37-source.md).

The remote bearer token grants full execution trust, including selecting any credential profile configured on that agent and any permitted target endpoint. It is not a tenant-scoped capability. Use separate agents and credential environments for separate trust domains.

During run preflight, before setup can recognize members or generate proofs, the timestamp-span block interval is compared with the XML expectation. `calibration.json` preserves both values, including on mismatch. The standalone `polkameter preflight` command performs the same check and includes successful calibration in its JSON result. A deviation above 25% prevents setup; the validated XML value remains the schedule/probe-budget input. Desktop run bundles are written to the operating system's Polkameter application-data directory under `runs/`.

The People plugin caches its read-only `check-state` connection and parsed state artifact. When a chain read fails, it reconnects once and repeats the read at the same block hash. Invalid artifacts and unknown claim hashes are not retried. If reconnection or the repeated diagnostic fails, the ordinary fail-stop policy applies; an unsuccessful diagnostic is not evidence that state is absent. Signing and submission are never retried by this diagnostic path.
