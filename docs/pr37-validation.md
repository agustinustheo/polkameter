# PR 37 migration validation

The migration was developed on branch `feat/th-pr37-plugins` from Polkameter `9b78224690c3ae9eda28a5f3ca24922660c9085d` and committed to `main` after `22659d593c4e25ff78f92a86c0651be456e86613`.

Upstream reference: `paritytech/polkadot-pop-e2e#37`, `8d2a060071c705654be8d3ae9dc668b4633469c0`. The capacity provisioner was previewnet-engine PR 39 at `e95c6a2ead177d8a7eb08dc839bdfc4af715c091`; the workflows now pin release `v0.7.0`, the first tag that contains it. Tests ran locally on macOS/Apple Silicon on 2026-10-03, using Rust 1.93.0 and node release `polkadot-weekly2026w40-rc1`. These measurements establish behavior on disposable local forks; they are not company-wide throughput claims.

## Current state (third review, 2026-10-04)

This section supersedes the pass counts and CI notes in the older sections below, which record earlier revisions.

- `cargo +1.93.0 test --workspace`: **105 passed, 2 ignored, 0 failed**. `pnpm test`: 13 passed. `cargo fmt --check`, the proof feature guard and v2 XSD validation of every example pass.
- `cargo clippy --workspace --exclude polkameter --all-targets -- -D warnings` passes and is now a CI gate. It covers the migrated PR 37 crates, the SDK, the engine and both plugins, which is the scope of upstream `stress/ci.sh`. The v1 desktop/CLI crate (`polkameter`) still has inherited warnings and is excluded. Tests run in the default profile, not `--release`; dependencies build at `opt-level = 3`.
- The People `check-state` diagnostic retries once on any chain read error, on a new connection and at the same block hash. The chain crate keeps upstream's error types.
- `people-default` expects 2-second blocks: it reproduces the upstream default dispatch (750 × 20, 6 + 4 tx/s for ten steps) on 3 People cores and 5 collators.
- `pr37-people.yml` and the E2E `stress-flood.yml` workflow share provisioning: system packages install without `sudo` when the runner is root, the frontend builds before Cargo, the fork is bitten with retries, `verify-previewnet-requirements.py` checks the exported requirements against the bitten bundle, and `ppn fork wait` gates the run. Each plan fixes its People cores and collators, so a plan cannot be started on a topology with the wrong block interval.
- The E2E workflow keeps a temporary branch `push` trigger (capacity plan), so it can run on its PR branch before it reaches E2E `main`. Remove that trigger before merging, as upstream planned.
- No live network run was repeated for these changes.

## Requested test pass (2026-10-04)

`cargo +1.93.0 test --workspace`: **105 passed, 2 ignored, 0 failed**. `pnpm test`: **13 passed**. Formatting, `git diff --check`, and `scripts/check-proof-features.py` passed. No new live network comparison, release build, Clippy run or transport-disconnection exercise was performed in this pass.

- Added `exit_code_policy`: stress health failures remain report-only; smoke monitor problems and required gaps fail; optional gaps and passing checks succeed.
- Replaced the two loss tests with `accounting_ledger_statuses_are_exclusive`: observable ledger rows cover pool presence, complete/incomplete loss evidence, unanswered submissions, fork-only inclusion, finalized dispatch failure, refusal and expiration. The table asserts exclusive accounting for all sent hashes. The existing reorg/tracker-view tests remain; pool-before-walk ordering remains supported by the retained live backlog evidence below.
- Added `offline_report_reproduces_retained_smoke_verdicts`: a 689 KB fixture from the retained 360-transaction smoke run feeds the CLI report path, which invokes `measurement::check`. Check names/statuses and the loss block match the recorded summary; `summary.md` is generated. Gzip compresses the filtered scrape stream without sampling away timestamps. See the fixture README for provenance.
- Merged the direct process-call test into `cancellation_stops_a_waiting_plugin`: the start event arms cancellation, the public `wait.pid` marker confirms the operation has begun, and the run returns 130/stopped with successful teardown, no ID mismatch, and no surviving PID.
- Extended `missing_credential_persists_failed_outcome_before_setup` to require `summary.md`.
- The existing single XML round-trip test already contains and preserves a comment and unknown attribute; it passed unchanged.
- Removed `slots_are_in_enum_order`, `mortal_era_matches_substrate`, unused `Era`/`GeneralTx::era`, and `published_recipes_have_pr37_budgets`. Existing encoded-byte vectors pass. Removed the superseded `pool_snapshot_covers_inclusion_after_the_walk_cutoff`, `missing_evidence_and_unanswered_submissions_cannot_prove_loss`, and `dropping_an_inflight_call_kills_it_without_poisoning_another_process` tests through the consolidations above.
- Skipped the conditional same-plugin teardown test: ordinary operation errors still terminate that plugin. `docs/plugins.md` documents this policy. Independent teardown steps remain attempted.

The cached People state reader reconnects once after a chain read error and repeats the diagnostic at the same finalized hash (see the current state above). Artifact errors are not retried, and neither signing nor submission uses this retry. The preflight calibration move and CI Rust pin were already present and remain covered by the existing checks.

### Replacement proposal (not activated)

Recommend a **repository XML-path workflow input**, rather than adding `run --set`. It introduces no override parser or second source of workload parameters. A reviewed plan contains mode, members, slots, rate schedule, recovery and interval expectation. `ppn_ref` remains a provisioning input; each plan fixes its People cores and collators. This deliberately replaces the upstream free `args` and separate `mode` inputs with a plan choice; it does not claim CLI argument compatibility. New ad hoc combinations require editing/adding a plan on the selected E2E ref. Smoke uses one core/one collator with a six-second expectation; capacity and default use three/five with a two-second expectation.

The E2E side lives in `paritytech/polkadot-pop-e2e` on branch `feat/th-polkameter-stress-flood`, based on E2E `main`: `.github/workflows/stress-flood.yml` plus the smoke, capacity and default plans under `stress/plans/`. It builds Polkameter and the People plugin from a pinned commit before provisioning, installs the plugin, credential and topology aliases, then runs `polkameter run "$PLAN" --output "$RESULTS_DIR"`. Fresh bite with retries, cores and collators from the plan, all-chain readiness, always-run summary and artifact steps and the network kill are kept from #37's workflow. The GitHub token is scoped to provisioning.

`POLKAMETER_REF` pins the Polkameter `main` commit that contains this migration; the workflow refuses anything but a full commit hash. The branch keeps a temporary `push` trigger (capacity plan) so the workflow can run before it reaches E2E `main`; remove it before merging. Agreement on plan selection and the health-equivalence acceptance gate is still required before #37's tool is retired.

Polkameter CI covers `cargo test --workspace`, a `clippy -D warnings` gate for the migrated crates, and the ark-vrf feature guard (plus frontend, v1 and schema checks). Tests use the default profile, not `--release`.

## Second-review fixes (2026-10-04)

Block-interval calibration now runs in both standalone preflight and the run's preflight, before setup can submit recognition or generate proofs. The run retains `calibration.json` even on a mismatch; standalone preflight returns successful calibration in its JSON output. Measurement no longer repeats this check after setup. The v2 example-validation CI command now explicitly selects `+${{ env.RUST_VERSION }}`.

Focused validation on Rust 1.93.0: both engine calibration tests and all 11 existing plugin-host integration tests passed with `--locked`; formatting and `git diff --check` passed. The new lifecycle regression uses an unavailable RPC endpoint to prove that calibration failure prevents setup while teardown still succeeds, and also checks standalone preflight rejection. The unit test separately covers invalid measurements and the 25% tolerance boundaries. No live network runs, full workspace suite, release build or Clippy were repeated for this follow-up; the retained live results below predate the timing change and are not new evidence for it.

Plugin documentation now explicitly includes ordinary operation errors in the process-termination policy. At that review, automatic reconnection of the cached People diagnostic client remained deferred (implemented in the subsequent requested test pass above); failed diagnostics cannot establish absent state. The health-equivalence and CI-cutover gate remains unchanged.

## Automated checks

- Workspace tests at the first revision: 103 passed, 2 ignored, 0 failed. Includes independent executable invocation, output references and scope isolation, timeout/cancellation, crash, invalid outputs, missing versions/operations, executable checksum, credential redaction, encoding/proof fixtures, rate budgets, loss evidence, reorgs and counter-reset semantics. The two ignored app tests require a dedicated fresh Polkadot dev node and an operating-system credential manager; the separate live v1 suite exercised the local node path.
- Frontend: 13 tests in 5 files, including XML round-trip and input-editor invalidation. Frontend production build passed. Vitest explicitly selects `src/**/*.test.ts` so archived upstream tests under Cargo's `target` are not accidentally included.
- The final workspace release build passed after the desktop/remote shared start guard and status fixes. Clippy passed earlier in the implementation; Clippy emits warnings, including inherited large-function/style warnings. The proof feature guard rejects nested `ark-vrf/parallel` pools.
- Existing v1 live Zombienet suite passed local and remote preflight, execution and report generation.
- Real authenticated remote v2 agent tests passed discovery, manifest inspection, execution, unauthorized rejection, v1/v2 overlapping-run rejection, cancellation, run-specific stop isolation, completed-phase clearing and retention of the latest 32 statuses. The final focused harness is retained at `target/check-agent-resume.py`; output is `/tmp/polkameter-pr37-resume-agent-check.log`.
- A copied smoke artifact bundle regenerated the same built-in summary offline (SHA-256 `9d0bba5d247635e8b4f0f062cb8b12c6ee309b62a332bbe4109e86407bfb2d83`) and six SVG plots.
- Example-plugin subprocess benchmark, 1,000 round trips: median 0.036 ms, p95 0.044583 ms, mean 0.04127 ms. This measures a small JSONL operation; it does not measure proof-generation capacity.

## Live People evidence

Paths below are relative to this worktree and intentionally remain under ignored `target/` rather than adding large network snapshots to Git.

| Run | Artifact directory | Observed outcome |
| --- | --- | --- |
| One real XML claim | `target/pr37-live-results/run-1791020385984-12564` | Recognition and claim finalized; finalized allowance assertion passed. |
| Migrated smoke | `target/pr37-live-results/run-1791020675891-18194` | 2/4/6 tx/s for 30 s each; 360 sent, accepted and finalized; zero refused, dispatch-failed, lost or unknown; 100 sampled allowances present. |
| Original PR release-independent reference smoke (debug build) | `target/pr37-upstream-results/stmt-flood-1791021317` | Same 360 transactions finalized, zero refused/failed/lost. |
| Migrated release smoke on a fresh fork | `target/pr37-fresh-results/run-1791022119169-52686` | Stopped on a 30 s block stall during the first rate step; 59 sent and finalized, zero lost. Recovery probe latency reached 48 s. Failed health verdicts retained. |

The first migrated smoke reused the fork after the one-claim test. The upstream smoke and repeated migrated smoke started fresh forks from the same published bundle. The first pair therefore demonstrates workload/accounting behavior but is not a controlled equivalence benchmark. The successful migrated smoke had a failed relay-slot check and a collation-funnel warning; the upstream run did not. The repeated fresh run did not reproduce the same healthy workload duration. Concurrent capacity-fork capture and differing setup durations further limit performance comparisons. No root cause for the repeated stall is claimed.

Stress-mode exit zero is inherited PR policy: it does not imply every chain-health check passed. The actual original silent-loss failure has not been reproduced in these smoke runs; deterministic accounting fixtures cover loss and incomplete evidence separately.

## Capacity comparison

A fresh fork was captured with 3 People cores, 5 People collators and 8 relay validators. Source heights: relay 226743, People 225254, Asset Hub 675765, Bulletin 225640, Collectives 225629. Two unrelated parachains did not advance on the local fork; relay and People did advance and finalize. The all-chain readiness wait was interrupted after verifying the required People/relay endpoints directly. This limitation applies to the comparison and must not be presented as a fully healthy five-chain deployment.

The upstream release run is `target/pr37-upstream-capacity/stmt-flood-1791023082`: rates 12/15/18 tx/s ran for 60 seconds each before the pool-intake rule stopped the ramp (p95 RPC reply 11.1 s). All 2,700 submissions finalized with no refusal, dispatch failure or loss, and 100 sampled allowances were present. Its 134-transaction backlog drained in 17 s; recovery probe latency returned within threshold in 20 s. The configured 21 tx/s step was not reached.

The migrated release run is `target/pr37-migrated-capacity/run-1791023508790-86021`, on a fresh copy of the same bundle. All four configured steps completed at 12/15/18/21 tx/s. All 3,960 submissions were accepted and finalized with no refusal, dispatch failure, unknown outcome or loss; 100 sampled allowances were present. The 45-transaction backlog drained in 3 s. The reported recovery point is 1 s (the start of the qualifying probe sequence, not the time all probes finished). Required health checks passed; the non-required voucher-to-root check had no result because this workload did not load vouchers.

`target/pr37-capacity-comparison.json` records matching configured workload parameters and different executed rates/verdicts. Setup timing and pre-run warm-up differed; some compilation overlapped the upstream run. The upstream capacity failure was **not reproduced; cause unknown**. These results do not establish performance equivalence or a speedup and cannot justify CI cutover. All owned capacity network processes were stopped after retaining the artifacts.

## Release acceptance and limits

The migration provides reusable XML composition, independent Rust preparation/state plugins, shared load/recovery/accounting services, and CLI/desktop/remote adapters. The People/relay/Recycler observer package and offline checks are currently bundled libraries. There is no dedicated external observer protocol or offline evaluator discovery API yet; custom plugin lifecycle operations can implement additional observations and evaluations.

The native macOS desktop application built with `RUSTUP_TOOLCHAIN=1.93.0 pnpm tauri build --debug --no-bundle`. The desktop workbench is covered by DOM tests. Interactive T3 browser verification was attempted but navigation/snapshot calls failed in the browser backend; no manual desktop interaction is claimed. Local builds and tests do not establish GitHub CI success.

The new manual People workflow and requirement-consumption script are reviewable adoption artifacts. The upstream E2E workflow and standalone CLI have not been retired. Cutover requires an accepted controlled comparison, successful CI on the intended runner, and agreement on the remaining observer/evaluator packaging boundary. Default 750-member live load and cross-platform release binaries have not been exercised here.

Prepared claims validate genesis, runtime/transaction version and period after proving and immediately before load. They are not continuously revalidated through a runtime upgrade or UTC period rollover; schedule runs within the prepared validity period. A future continuous validity guard should stop load when that identity changes.

## Review repairs

The retained runs above predate the reconciliation-order fix. Because their backlogs drained completely, they do not validate that fix. New regression and matched-run evidence belongs below; previous pass counts describe the earlier revision until replaced by the final repair checks.

At the repair revision, 107 active Rust tests passed (2 ignored); later consolidation reduced the count (see the current state above). New cases cover the ordered pool/head snapshot, calibration mismatch, a dropped in-flight call followed by independent plugin cleanup, and recovery after an application-task panic. Crash diagnostics are asserted in the retained plugin stderr log. The release workspace builds with `--locked`, and the new root lockfile is staged.

The short-recovery run `target/review-live/backlog/results/run-1791039197041-23054-1` submitted 300 claims with only one second of recovery. Reconciliation recorded 147 finalized and 153 in the earlier pool snapshot, zero lost and zero unknown. All 153 pool-snapshot hashes subsequently appeared in finalized blocks after cutoff 225317. The receipt is `backlog-validation.json`; `accounting-validation.json` verifies exclusive per-hash accounting. The best-chain observation count in `loss.included` was only 49, illustrating why it must not substitute for finalized-chain accounting.

### Repeated capacity comparison after repairs

The harness `target/review-live.py` ran five isolated networks sequentially: the backlog case, migrated 1, upstream 1, upstream 2, migrated 2. Each used a fresh copy of the same captured bundle, the same binaries/topology, declared-chain readiness and a fixed 60-second warm-up. No builds or other test networks ran concurrently. Seeds and exact setup duration still vary between runs. The two unrelated stalled parachains described above remain a limitation.

| Run | Executed rates (tx/s) | Sent / finalized | Stop | Lost / unknown | Recovery / backlog drain |
| --- | --- | --- | --- | --- | --- |
| Migrated 1 | 12/15/18/21 | 3960 / 3960 | rate cap | 0 / 0 | 28 s / 30 s |
| Upstream 1 | 12/15/18/21 | 3960 / 3960 | rate cap | 0 / 0 | 1 s / 3 s |
| Upstream 2 | 12/15/18/21 | 3960 / 3960 | rate cap | 0 / 0 | 1 s / 3 s |
| Migrated 2 | 12/15/18/21 | 3960 / 3960 | rate cap | 0 / 0 | 1 s / 3 s |

All four runs satisfy exclusive accounting (`sent = finalized + refused + expired + inPool + lost + unknown`), with identical executed rates and stop rule/class. `target/review-live/acceptance.json` records the checks; `comparison-1.json` and `comparison-2.json` retain the paired differences. Upstream has no explicit unknown field, and its observed accounting has zero remainder.

Health verdicts are not identical. Migrated 1 recorded a relay-slot failure and build-time/PVF/collation warnings. Upstream 1 had none of those non-pass checks; upstream 2 recorded relay-slot and burst-drain failures; migrated 2 recorded a burst-drain failure. Both implementations therefore show run-to-run health variation. The previous pool-intake failure was not reproduced; cause unknown. The batch supports workload, stop-rule and accounting agreement, **not full performance/health equivalence or an automatic CI cutover**.

### Review disposition

| Finding | Disposition |
| --- | --- |
| H1 | Fixed pool-before-walk ordering, finalized cutoff coverage, unit regression and live backlog-follow-up proof. |
| H2 | Removed equivalence implications; retained all differences and the no-cutover gate. |
| M1 | Every deliberate evidence/metrics/timing deviation is listed in `pr37-source.md`. |
| M2 | An in-flight guard kills dropped calls immediately and keeps their protocol state failed; independent cleanup remains usable. |
| M3 | Timestamp-span measurement and a 25% mismatch gate now run during preflight, before setup; both capacity calibrations measured exactly 2.0 s. |
| M4 | Legacy colliding targets remain for comparison. Unique targets need a separate workload version and baseline; the weak state-check interpretation is documented. |
| M5 | Panics become failed statuses and release the run slot; desktop artifacts use the OS application-data directory. |
| M6 | Run-time stderr is retained per plugin with Unix owner-only file permissions. |
| M7 | One client and one hash-pinned parsed state artifact are cached for repeated People checks. |
| Lockfile/token | Root lockfile staged; `--locked` release build passed; GitHub token scoped to provisioning. |
| Simplification | Removed five rule wrappers, the duplicate measurement preflight, unused People dependencies/hash helper, six exploratory examples, duplicate editor listener and empty telemetry placeholder. V2 HTTP requests share a pooled client/helper; all frontends share report-failure handling. |
| Already present | CI already validates every v2 example with the v2 XSD. Both People workflows check the exported requirements against the bitten bundle. |
| Retained scope | Desktop, remote and workflow interfaces remain because they were explicitly requested. Explicit inspection may launch a separate short-lived process; replacing it with a manifest cache is deferred until warranted. |

The remote token's full-trust authority over configured credentials and chosen endpoints is documented. No changes were committed or published, and the original Polkameter checkout's unfinished merge was not modified.

Final repair regressions: 13 frontend tests and production build passed, including comment/unknown-attribute preservation during XML editing; unsupported attributes remain rejected by host validation. The unchanged v1 Zombienet smoke suite passed fresh local and remote execution/reporting at `target/review-v1-smoke`. The real-agent harness waited for the plugin's slow-call PID marker, cancelled it, verified that PID had exited, and found no request-ID mismatch; it also rechecked exclusion and status history. Schema and proof-feature guards passed. A copied backlog bundle replayed to an identical summary JSON (SHA-256 `247c5e46d6bbc37227cb0fb00e3f4055afe66e88a92dba9fdc26e89f25050849`).
