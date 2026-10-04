# Polkameter XML scenarios and reusable plugins

## Goal

Make Polkameter a shared non-functional testing framework for teams across Parity. Use JMeter's separation of test plans and executable components as the architectural model: versioned XML scenarios compose reusable steps, while independently packaged plugins implement custom preparation, proof generation, transaction authorization, observations and assertions. Rust is the first plugin implementation language. A versioned protocol should allow other languages later without changing the scenario model.

Use `paritytech/polkadot-pop-e2e` PR #37 as the first complete adoption case. Express its `stmt-flood` scenario in XML and move its People-specific code into a reusable plugin. Polkameter must prepare claims, establish a baseline, apply increasing transaction rates, observe recovery, reconcile outcomes against the finalized chain and produce reproducible checks and reports. New combinations of existing operations should require only a scenario edit; new runtime-specific behavior should require a plugin change. Changes to the core should be reserved for new framework capabilities.

The implementation now lives in the isolated `feat/th-pr37-plugins` worktree. See [the implementation guide](plugins.md) and [validation evidence and remaining acceptance limits](pr37-validation.md). This document records the intended architecture and migration sequence; the status below distinguishes implemented behavior from release acceptance.

## Lessons from JMeter and the discussion

XML describes composition: which component runs, its configuration, its scope and where its inputs come from. Executable components perform the work. JMeter provides samplers, controllers, configuration elements, processors, assertions and listeners; its pre-processors can perform work before a sampler, and its scripting samplers can perform custom computations. Polkameter should adopt these responsibilities with documented semantics appropriate to asynchronous blockchain execution. See [JMeter test-plan elements](https://jmeter.apache.org/usermanual/test_plan.html), [JSR223 sampler](https://jmeter.apache.org/usermanual/component_reference.html#JSR223_Sampler) and [JMeter plugin development](https://jmeter.apache.org/usermanual/jmeter_tutorial.html).

| JMeter concept | Proposed Polkameter responsibility |
| --- | --- |
| Configuration | Named endpoints, credential references, runtime profiles and plugin parameters |
| Controllers and timers | Ordered steps, iterations, user concurrency and transaction-rate schedules |
| Setup and pre-processors | Once-per-run preparation and explicitly scoped work before a sample |
| Samplers | Submit prepared extrinsics, perform RPC operations or execute a custom measured action |
| Post-processors and assertions | Extract outputs, verify state and evaluate recorded evidence |
| Listeners | Persist events and samples and present reports |

This proposal adopts JMeter's component model. Polkameter remains a Rust engine with its own XML schema; importing arbitrary JMX plans or Java plugins would be a separate compatibility project. XML and JSON both support nested structures. XML is the chosen authoring format because it fits Polkameter's existing plans, not because JSON lacks nesting. JMeter supports CSV/XML sample results; JSONL is Polkameter's event format, not a JMeter requirement. [JMeter listeners](https://jmeter.apache.org/usermanual/test_plan.html#listeners)

OpenMetrics is complementary: it specifies a metrics data model and exposition formats, while the scenario describes execution. Keep raw events and traces as evidence alongside numeric metrics. The transcript's references to an uncertain API/library name do not establish a dependency; verify the intended library before adopting it. [OpenMetrics specification](https://prometheus.io/docs/specs/om/open_metrics_spec/)

## What the two projects actually do

Polkameter is **JMeter-inspired**, with its own Rust/Subxt execution engine. It does not execute Apache JMeter. Its JMX export contains thread-group structure, while Substrate samplers remain in the Polkameter plan. Its importer inspects JMeter structure. The migration therefore extends an existing Rust engine; it does not require translating the PR into Java samplers. See [Polkameter's architecture](../README.md), [JMX handling](../src-tauri/src/jmx.rs) and [submission adapter](../src-tauri/src/subxt_adapter.rs).

There is real overlap in scheduling, RPC access, telemetry, CLI orchestration and reporting. The PR also adds substantial behavior that Polkameter does not currently provide. That behavior is the useful contribution to preserve.

The reviewed source is [PR #37](https://github.com/paritytech/polkadot-pop-e2e/pull/37), branch `stress-tests-mvp`, commit `8d2a060071c705654be8d3ae9dc668b4633469c0`. Polkameter was inspected locally at HEAD `9b78224690c3ae9eda28a5f3ca24922660c9085d`. Its checkout has existing conflicts in `package.json`, `src-tauri/Cargo.toml` and `src-tauri/Cargo.lock`, plus a staged release-workflow change. Those files were left untouched. The workspace supplied for this discussion is `individuality-community`; the PR belongs to the separate `polkadot-pop-e2e` repository.

## Capability map

| Capability | PR #37 | Polkameter today | Migration |
| --- | --- | --- | --- |
| User entry points | Dedicated `stress` CLI and workflow | Desktop editor, CLI and remote agent sharing Rust core | Use Polkameter's entry points |
| Transactions | Prepared v5 general extrinsics with `AsResources` ring proofs; custom setup signing | Dynamic calls signed through `PolkadotConfig`; runner rejects custom profiles | Add a transaction-provider interface and People adapter |
| Workload preparation | Recognize people through sudo, wait for finalized rings, generate proofs and reserve probes | Ordered setup/workflow/teardown calls and development funding | Add preparation hooks that produce fresh transactions and scenario context |
| Load generation | Sustained tx/s steps, additive or geometric growth, token bucket and multiple RPC connections | Burst, ramp and Poisson **user-start** offsets; each user's workflow executes sequentially | Add transaction-rate scheduling independent of inclusion latency |
| Stop conditions | Capacity measures, pool refusal/intake, block/finality stalls, exhausted budget and generator limits | Sample timeouts, whole-run timeout and cancellation | Add explicit stop rules and separate generator limitations from chain failures |
| Recovery | Reserved probes, latency relative to baseline, block cadence and backlog drain | Drain active work on cancellation | Add a recovery phase and separate recovery/drain results |
| Transaction accounting | Hash tracker, block/event reads, reorg backfill, finalized-chain reconciliation, pool inspection and state sampling | Submitted/in-block/finalized sample outcomes | Add run-wide reconciliation and scenario state checks |
| Observability | Multiple nodes from Zombienet topology, relay/PVF, pool, block weights, Recycler and process metrics | Chain counters, runner process metrics and one optional node Prometheus endpoint | Add labeled monitor providers and topology discovery |
| Evaluation | Offline checks with pass/warn/fail/info/no-result, structured summary and failure classes | JTL/JSONL artifacts, Markdown summary and SVG plots | Extend the artifact contract and report evaluator |
| CI semantics | Stress measures degradation; smoke rejects observability gaps | Failed/timed-out samples make `run` exit nonzero | Add explicit measurement policy while retaining existing run behavior |

Sources: the PR's [stress overview](https://github.com/paritytech/polkadot-pop-e2e/blob/8d2a060071c705654be8d3ae9dc668b4633469c0/stress/README.md), Polkameter's [scenario model](../src-tauri/src/scenario.rs), [scheduler](../src-tauri/src/scheduler.rs), [runner](../src-tauri/src/runner.rs), [telemetry](../src-tauri/src/telemetry.rs) and [reporting](../src-tauri/src/report.rs).

## Plugin architecture

Polkameter owns the XML interpreter, run lifecycle, scheduling, common transport, transaction tracking, artifact storage and evaluation orchestration. Plugins implement operations behind a stable contract. A team installs a plugin and names its operations in XML without adding a scenario-specific branch to the runner. Built-in steps and plugin steps obey the same input, output, error and lifecycle rules.

The recommended first implementation uses a long-lived plugin executable with a versioned JSONL protocol over stdin/stdout and a Rust SDK. Protocol messages carry request IDs; ordinary logs go to stderr. Start one process per plugin per run, with explicit invocation scope and concurrency limits. Prepare or transfer transactions in batches; never launch a process for each extrinsic. Rust fits the existing proof code, but a future TypeScript executable can implement the same protocol. This packaging choice is proposed, not an existing Polkameter feature, and its throughput must be measured.

The People plugin owns person creation, ring discovery, proof generation, claim context, v5 extension encoding and allowance-state checks. Generic code receives prepared transaction bytes, a hash, workload identity and descriptive metadata. A proof-authorized claim must not be forced through the existing assumption that every sample has a funded sr25519 signer. Setup credentials and workload authorization are separate concerns. Where signing and proof construction must share a payload, keep them together in a plugin operation; do not impose a universal signing step on every transaction kind.

The minimum contract includes:

- **Discovery and compatibility.** A manifest declares plugin ID, version, protocol version, operations, input/output schemas, concurrency support and environment requirements. Plans pin plugin versions; a local installation registry resolves them to executables. CI records package checksums. Unknown operations, incompatible versions and invalid input references fail before setup. Installing an independent plugin must not require rebuilding Polkameter.
- **Invocation and data flow.** Each step has a unique ID, operation, typed inputs, deadline and declared output names. Outputs become available only after successful completion and can feed later steps. Run-level preparation outputs are immutable; per-user and per-iteration values have isolated scope. Initial control flow is ordered steps and explicit bounded iteration. XML references named values rather than embedding arbitrary source code.
- **Workload data.** Small outputs are structured values. Large proof batches and prepared extrinsics use versioned, checksummed artifact references or bounded streams. A shared consuming source allocates each transaction at most once across workers. Flood and probe reserves are separate. Runtime identity, expiry and scenario metadata travel with prepared data. The SDK validates records before the host submits them.
- **Host services.** Plugins can read declared RPC targets and request tracked setup transactions through host services. The host owns load scheduling and submission accounting. Generic custom samplers may perform other actions, but extrinsics counted in the load must use the host submission path. A plugin cannot report an opaque internal flood as equivalent to individually tracked transactions.
- **Lifecycle and failures.** Distinguish manifest validation, environment requirements, live preflight, run setup, per-sample work, background observation, evaluation and teardown. Support progress, structured errors, cancellation, timeout and bounded shutdown. Attempt cleanup after partial setup. Never automatically retry a state-changing operation after an ambiguous failure. A plugin crash is a tool failure, distinct from a chain-health verdict.
- **Credentials and execution.** XML contains credential aliases. The executing host resolves only the declared credentials for a plugin, keeps secret values out of logs/artifacts and runs the selected installed executable without shell interpolation. Plugins are trusted code; process separation is not a security sandbox. Remote workers resolve their own installations and credentials.

## XML composition example

The executable [generic plugin workflow](../examples/plugin-workflow.polkameter.xml) calls an independently packaged Rust plugin and passes its typed output to another step. [The one-claim plan](../examples/people-one-claim.polkameter.xml) composes recognition, finalized setup submission, proof generation, claim submission and a finalized-state assertion. The [smoke](../examples/people-smoke.polkameter.xml), [capacity](../examples/people-capacity.polkameter.xml), and [default](../examples/people-default.polkameter.xml) plans add rate schedules, monitor requirements and configurable thresholds.

Use the XML v2 namespace and exact installed plugin version shown in those files. The [schema](../schemas/polkameter-plan-v2.xsd) and host contract validation define the implemented syntax. Step preparation timings are recorded separately from load submissions.

## Monitoring and Previewnet integration

A monitor plugin declares required evidence before the network starts: endpoint roles, RPC methods, metric families and labels, trace/log sources and node instrumentation capabilities. A separate Previewnet provisioner consumes the combined requirements and returns resolved endpoints, node identities, runtime versions and a record of supported capabilities. Polkameter also supports an already-running network by checking the same requirements. Provisioning remains an integration boundary, not behavior hidden in a transaction sampler.

Define a small initial evidence catalog from PR #37: best/finalized block progress, submitted/accepted/included counts, pool readiness and backlog, block construction/weight, PVF observations, process resources and allowance state. Each entry needs a source, unit, node scope, collection cadence, phase window and missing-data policy. Specialized trace parsers belong in plugins. Unsupported required instrumentation blocks a measurement before load; evidence lost during a run produces an explicit incomplete result.

This reconciles the discussion's two needs: agree on stable interfaces now and refine the metrics catalog as scenarios expose new questions. The framework standardizes how evidence is requested, collected and evaluated; each check supplies its runtime-specific interpretation. Block-stall detection must use a configured or observed expected interval and a tolerance, rather than treating six seconds as a universal rule. Assertions should cite the evidence and thresholds behind their verdicts.

## Source migration map

Use the following source map. Paths in the first column are relative to the PR's `stress/crates/` directory; destinations are proposed responsibilities, not existing APIs.

| Source to reuse or adapt | Destination in Polkameter |
| --- | --- |
| `proofs/`, `scenarios/src/shared/`, `scenarios/src/stmt/`, relevant `chain/src/{tx,setup,reads,value}.rs` | Independently packaged People plugin operations, including compatibility tests |
| `load/src/{source,plan,steps,rules,recovery}.rs` | Generic prepared-workload and rate-plan support behind the existing runner |
| `load/src/sender.rs`, `submit.rs`, `tracker/`, `follower.rs`, `loss.rs` | Transport and independent transaction-observation services |
| `monitors/src/{topology,scraper,process,relay,walker}.rs` | Generic or Polkadot-specific monitor providers with explicit endpoint roles |
| `monitors/src/recycler.rs` and scenario-specific state reads | People monitor/assertion plugin operations |
| `files/`, `checks/` | Versioned evidence records and offline outcome evaluator integrated with current artifacts |
| `cli/src/{run,wiring}.rs` | Reference for lifecycle integration into current application/runner code |
| `.github/workflows/stress-flood.yml` in the repository root | Previewnet integration recipe invoking Polkameter |

Avoid maintaining a second standalone runner inside Polkameter. The PR's reusable pieces can be extracted into small libraries and adapted incrementally. Preserve source attribution, license notices and a record of the upstream commit for copied code.

## Implementation sequence

1. **Prove the XML plugin boundary.** Define the operation manifest, protocol, Rust SDK and next plan schema with v1 compatibility. Implement discovery, typed output references, run/per-user scopes, invocation, cancellation and errors. Add an example plugin outside the core package: an XML step calls it and passes its output to a later step. Verify this through the existing headless CLI, including a subprocess integration test. This milestone proves extensibility without changing the core for each operation.

2. **Prove one real claim through XML.** Keep the current signed-call path as a built-in provider. Add prepared-transaction submission and the People plugin, porting the encoder, proof generator and setup helpers with the existing Rust/TypeScript vectors. On a disposable Previewnet fork, execute XML steps that prepare a person, wait for a built ring, prepare and submit a proof-authorized claim and verify its finalized allowance state.

3. **Add transaction-rate plans and accounting.** Support explicit rate steps plus additive and geometric generation. Separate submission from block observation so waiting for finality does not throttle the requested rate. Record requested, actually sent, accepted, included and finalized counts separately, with RPC-reply and inclusion latency. Port stop rules, bounded buffering/backpressure behavior, generator-limit detection and source exhaustion. Measure plugin transport overhead. Keep the existing user-workflow scheduler available for its current workloads.

4. **Add baseline, recovery and loss reconciliation.** Run the sequence `resolve plan and requirements → provision or attach → live preflight → preparation → baseline → load → recovery → reconciliation → checks → cleanup and report`. Start required monitors before their observation windows and retain them through recovery/reconciliation. Reserve enough unique claims for baseline and recovery before starting load. Follow block bodies and dispatch events, fill gaps after reorgs and reconcile by transaction hash on the finalized chain. Inspect the submission node's pool and run plugin state assertions. Report incomplete evidence explicitly instead of counting it as success or confirmed loss.

5. **Bring over monitors and offline evaluation.** Implement the requirements contract with Previewnet and support the PR's required block-production, PVF, pool and recording checks, plus applicable Recycler checks. Preserve node identity, metric labels, phase boundaries, counter-reset detection and missing-data semantics. Keep JTL and the existing portable bundle, adding versioned records for plugin invocations, steps, raw scrapes, blocks, reconciliation, lost-transaction details and structured verdicts. Retaining the PR's raw records and OpenMetrics conversion initially can reduce the risk of changing measurement semantics. Offline evaluator plugins must consume recorded evidence without chain access; the bundle records their exact versions and checksums.

6. **Expose plugins in desktop and remote execution.** Update TypeScript types and the editor to use plugin manifests for configuration forms and validation. Preserve plugin steps and typed references on XML open/save. Expose phase progress and results through the shared application layer. Remote agents advertise installed plugin/protocol versions and resolve credentials locally. Missing worker capabilities fail before a run. Existing v1 plans must still load, and JMX remains a structural companion.

7. **Establish equivalence and switch CI.** Start from equivalent fresh forks and pinned node/runtime/topology versions for each implementation, since setup and load consume chain state. Run both the short smoke recipe and the focused capacity recipe below. Compare workload shape, actual offered rate, accounting, recovery and verdict evidence. Then have the E2E repository's workflow install the required plugin and invoke Polkameter with the scenario XML. Retire the duplicated CLI once acceptance criteria pass.

## Semantics that must survive the migration

- **A successful RPC submission is not a successful chain operation.** Preserve later dispatch failures, transactions seen only on abandoned forks, accepted transactions absent from finalized blocks and the ready pool, and included operations lacking expected state. Distinguish an observed accounting gap from a proven internal node cause.
- **Metadata encoding alone cannot build this workload.** The PR puts the proof in `AsResources`, outside ordinary pallet-call arguments. Its encoder assumes a particular v5 extension layout, and the claim uses fixed pallet/call indexes. Retain extension-order validation and add explicit call/type compatibility checks before load. Record runtime version, genesis and adapter version. A `Custom` profile string alone does not provide an implementation.
- **Proof generation belongs before timed load.** Preserve the `verifiable = 0.3.0` compatibility baseline and the feature guard against nested `ark-vrf/parallel` pools until dependency changes are validated. Track setup/proving time separately. Check that prepared claims remain valid for the run's period and runtime.
- **A user-start ramp is not a transaction-rate ramp.** A workflow can contain many calls and wait for responses between them. Reusing the existing arrival configuration alone will change the workload. Measure scheduling lag and delivered rate so a saturated load generator does not look like the chain's capacity limit.
- **A measurement verdict and process failure are different.** In the PR, stress mode can exit zero with failed chain-health checks. Smoke additionally rejects monitor problems and required checks without results; it does not make every failed health check a process error. Make this policy explicit and retain Polkameter's current failure behavior for existing plans.
- **Budgets, timeouts and metric scope matter.** The default PR ramp takes up to 600 seconds before up to 900 seconds of recovery, excluding preparation and reconciliation. Polkameter's default whole-run timeout is 900 seconds. Provide suitable lifecycle budgets. Keep probe transactions separate from measured flood traffic and node-local metrics separate from network aggregates.

## Acceptance criteria

- An independently built Rust plugin is discovered and called by an XML step without rebuilding or editing the core. Its typed output feeds both a built-in step and another plugin step. A second simple plugin proves that the interface contains no People-specific assumptions.
- Tests cover once-per-run setup, per-user/iteration isolation, deadlines, cancellation, crashed plugins, invalid outputs, unknown operations, unavailable versions and cleanup after partial setup. Ambiguous transaction submission is not blindly retried.
- Changing rates, members, slots, thresholds or combinations of installed operations requires only XML edits. CLI and desktop execute the same resolved plan, and desktop open/save preserves plugin configuration and output references.
- Monitor requirements reach the Previewnet integration before startup. Missing capabilities, mid-run missing data and node restarts remain distinguishable from successful health checks. Stall thresholds are configurable for the target chain.
- The migrated encoder passes the existing claim-byte, hash, person-generation and proof cross-validation fixtures. Unsupported runtime layouts fail before load.
- A real claim succeeds through Polkameter and produces the expected finalized state on a disposable People fork.
- The smoke plan supports 40 members, 12 slots, rates 2/4/6 tx/s for 30 seconds each, a 120-second recovery budget and 3 baseline probes, matching the PR's documented recipe.
- The focused CI plan supports 250 members, 20 slots, rates 12/15/18/21 tx/s for 60 seconds each, a 900-second recovery budget and 5 baseline probes. Its topology is 3 People cores and 5 People collators. The implementation also supports the PR's default 750-member workload and geometric rates.
- Requested and delivered load, refusals, inclusion failures, pending work, finalized inclusion and loss evidence can be reconciled by hash. Tests cover reorgs, missed block observations, pool-read failure, incomplete finality and accepted transactions that never appear in either finalized blocks or the observed ready pool.
- Recovery latency and backlog drain are reported separately. Exhausted claims and generator saturation produce explicit stop reasons.
- Offline evaluation reproduces verdicts from retained artifacts. Missing required observations and counter resets yield no-result outcomes. Node aggregation tests prevent multiplying workload counts by the number of collators.
- Controlled failure fixtures prove silent-loss detection even if the live node bug is fixed. A live comparison reports whether the original failure reproduces; it does not assume a fixed breaking rate across machines or runtime versions.
- Existing signed-call plans, artifact reports, desktop execution, headless execution and remote execution retain their supported behavior. New scenario capabilities work through the shared core.

## Validation status

The shared XML v2 engine, independent Rust SDK/plugins, People preparation, rate controller, recovery/reconciliation, bundled monitor/check libraries, reports, CLI, desktop workbench and authenticated remote routes are implemented. Existing v1 local and remote execution passed live regression tests. A real finalized People claim, a 360-transaction smoke run and the full 3,960-transaction capacity recipe succeeded; another fresh smoke run stopped on a block stall, retaining the failed health evidence. Capacity runs used the same captured fork and topology, but differing warm-up and local resource contention prevent a performance-equivalence claim.

The bundled People/relay/Recycler monitor package is not yet an independently installed observer executable. Built-in checks replay offline; external evaluator discovery is not a separate offline plugin API. Arbitrary custom observers/evaluators can be written as plugin lifecycle operations. The original E2E workflow and CLI remain intact pending controlled equivalence acceptance and company review. Local results do not establish GitHub CI status or production throughput. See [the validation record](pr37-validation.md) for exact runs and outstanding acceptance limits.

## Independent-review repair plan

1. Restore temporally consistent pool/finalized-chain reconciliation and retain the live backlog evidence for inclusion after the walk cutoff. Cover exclusive ledger statuses with a table test. Treat incomplete pool, cutoff or finality evidence as unknown.
2. Make dropped plugin calls kill their process, preserve stderr diagnostics, and test an interrupted call followed by independent cleanup. Catch application-task panics and use the OS application-data directory for desktop artifacts.
3. Measure and validate the configured block interval during preflight, before recognition or proving in setup. Remove redundant monitor preflight work, cache People loss-diagnostic inputs, and remove unused wrappers/dependencies.
4. Include the root lockfile, scope the workflow token to provisioning, and document every semantic deviation and the agent's full-trust credential boundary.
5. Validate with focused regression tests, v1 regression, and fresh matched-fork comparisons with identical warm-up and no concurrent builds. Include a deliberately short recovery with a remaining backlog. Accounting and stop/verdict differences remain visible; they must not be waved through as equivalent.

Keep the requested desktop, remote, workflow and provisioning interfaces in this worktree. They are already consumers of the shared engine; deleting them is a scope change rather than a correctness fix. Defer unique People targets, prover controls and external observer packaging to separately measured changes. Do not cut over CI on the existing unmatched capacity evidence: the upstream failure was **not reproduced; cause unknown**.
