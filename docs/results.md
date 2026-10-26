# Run directory

A run writes a directory of raw files. [Read the results](reading-results.md) explains how to interpret them. A plan with a `<load>` element is a load test; other plans write fewer files.

## The run directory

Example plan, no load test (plot files trimmed):

```text
run-1792253315025-76883-1/
  events.jsonl
  execution.json
  plan.json
  plots/                      throughput.svg, latency-percentiles.svg, failure-breakdown.svg
  plugins/example/stderr.log
  resolved-plan.json
  samples.jtl
  summary.md
```

The name is `run-<milliseconds>-<pid>-<sequence>`, created in `--output`, or `--output-root` on a remote agent. A run never reuses a directory.

## Files in every run

| File | Contents |
| --- | --- |
| `execution.json` | How the run ended. |
| `summary.md` | No load test: plan name, state, and error or `All configured steps completed.` Load test: the full report. |
| `events.jsonl` | Every event, one JSON object per line. |
| `plan.json` | The plan as parsed. |
| `resolved-plan.json` | Plugin manifests and registry records, `<monitors>` settings, CLI version and BLAKE2 hash. |
| `samples.jtl` | JMeter CSV: a row per step, and per transaction in a load test. |
| `plots/*.svg` | Samples per second; p50, p95, p99 of `elapsed`; failed samples by `responseCode`. |
| `plugins/<id>/stderr.log` | Standard error of each plugin started. |

No file stores a credential value. Where a plugin or error would echo one, the file shows `[redacted]`.

## Files in a load test

| File | Contents | Needs |
| --- | --- | --- |
| `calibration.json` | Configured and measured block interval; 25% tolerance. | |
| `summary.json` | The summary as data. | |
| `steps.jsonl` | Per step and lane: target rate, sent, included, refused, p50 and p95 latency, node CPU and memory. | |
| `load.jsonl` | The load tool's `polkameter_*` metrics. | |
| `blocks.jsonl` | Each best block, and the gap since the previous one. | |
| `transactions.jsonl` | Each sent transaction and its [status](#loss-accounting). | |
| `lost.jsonl` | Each lost transaction, the first 1000. `loss.lost` holds the full count. Empty if none. | |
| `plugin-checks.json` | Checks from evaluate steps. Empty if none. | |
| `resolved-targets.json` | Scrape targets resolved from the topology, and its hash. | `<monitors>` |
| `scrapes.jsonl` | Raw `/metrics` per node; `error` if a node did not answer. | `<monitors>` to hold records |
| `chain.jsonl` | The relay recorder's series for the observed parachain. | `<monitors>` to hold records |
| `node.jsonl` | Node CPU and memory during the load. | `POLKAMETER_NODE_PID`, or the node's RPC port listening |
| `plugins/<id>/series.jsonl`, `metrics.json` | A plugin's series and metric declarations. | A plugin that records metrics |

## execution.json

Written after teardown.

| Field | Meaning |
| --- | --- |
| `artifact_dir` | Absolute path of the run directory. |
| `exit_code` | The code the `run` command returns. |
| `state` | How the run ended, as below. |
| `error` | Error text, redacted. `null` if none. |

| `state` | `exit_code` | When |
| --- | --- | --- |
| `completed` | `0` | No error. |
| `completed_with_failures` | `1` | A `FAIL` verdict with `--fail-on-verdict`, or a smoke-mode monitor problem or required check with no result. |
| `stopped` | `130` | Ctrl-C, or a remote stop request. |
| `failed` | `1` | Any error, including a failed `core.assert-equal` step and a report that could not be written. |

## summary.md

Sections of a load test's `summary.md`:

| Section | Contents |
| --- | --- |
| `### <plan name>`, `#### Artifact under test`, `#### Stress method` | Title; what the transactions exercise; the method and step criteria. |
| `#### Result` | Verdict; Failure onset (`breakingPoint`); Max sustained (`maxSustained`); Load stop (`stop`); Recovery (`recovery`); Loss check (`loss`). |
| `#### Load steps` | One row per step, then one for recovery. |
| Collator metrics | Per step and recovery: node CPU and memory, block build time, pool work, why blocks ended. |
| Recovery probes | Probes sent after the load, and when each landed. |
| `### Outcome checks` | One row per check: outcome, check, status, detail. |

## summary.json

The measurement writes `summary.json` first, so plugin checks can read it. The report rewrites it with the checks and verdict added, and writes `summary.md` only then.

A plan without a load test writes no `summary.json`, so it has no verdict. Older summaries and the initial summary before checks have no `verdict` key.

The `verdict` object from the recorded smoke fixture, regenerated with `polkameter report`:

```json
{
  "result": "FAIL"
}
```

| Key | Holds |
| --- | --- |
| `schemaVersion` | Layout version, `1`. Existing fields keep their layout. |
| `verdict` | `result`: `PASS`, `WARN`, `FAIL` or `INCONCLUSIVE`, matching `summary.md`. Present after the report adds the checks. |
| `scenario`, `runId`, `mode` | Plan name, run ID, `smoke` or `stress`. |
| `artifact`, `params`, `budget`, `setupSeconds`, `extra` | Artifact, plan options, prepared transactions, setup time, chain genesis and transaction and node versions. |
| `rules` | Effective thresholds, camel case (`minIncludedRatio`). |
| `stop` | `rule`, `detail`, `step`, `class`. |
| `breakingPoint` | `step`, `targetRate`, `measure` (`included`, `latency` or `refused`), `detail`. `null` if none. |
| `maxSustained` | `step`, `targetRate`, `includedPerS` of the step before. The last step if none. |
| `failureModes` | `class` and `what` per failure. See [Failure modes](#failure-modes). |
| `recovery` | `measured`, `recovered`, `seconds`, `backlogAtStop`, `backlogAtEnd`, `drainedSeconds`, `drainPerS`, `probes`, `blocks`, `node`. |
| `loss` | Transaction counts. See [Loss accounting](#loss-accounting). |
| `baseline` | `probes`, `p50Ms`, `maxMs` of the probes before the load. |
| `problems` | What the monitors could not record. |
| `network`, `runner` | Node URL, parachain ID, runtime spec version, block interval; CPU count and model. |
| `steps` | Number of load steps. |
| `checks` | `outcome`, `check`, `status`, `detail`, `numbers` if any. `"optional": true` on optional checks only. |

The verdict uses the existing stop, failure modes, breaking point and checks. `breakingPoint` gives the failure onset; `maxSustained` gives the preceding step. Reasons remain in `stop.detail`, `breakingPoint.detail`, `failureModes` and `checks`, without copies inside `verdict`.

[Read the results](reading-results.md#did-it-pass) lists the verdict rules. `--fail-on-verdict` changes the run's exit policy only; it does not change these rules.

## Loss accounting

Each sent transaction gets the first status below that holds.

| Status | Meaning |
| --- | --- |
| `finalized` | In a finalized block the run read. `failedInFinalizedBlock` says if dispatch failed. |
| `in_pool` | Ready in the node's pool. |
| `refused` | Refused by the node at submit. |
| `lost` | Accepted or seen in a best block, in no finalized block, not refused, not ready in the pool. Decided only after the finalized chain and pool are read. |
| `unverified` | In a best block, finality not verified: the wait timed out, or the chain could not be walked. |
| `unknown` | No evidence: no reply or inclusion, or the chain or pool is unreadable. |

The `loss` object in `summary.json`:

| Field | Meaning |
| --- | --- |
| `sent` | Transactions sent; the statuses add up to it. |
| `accepted` | Submits with a successful RPC reply, not dispatch successes. |
| `included` | In a finalized block the walk read. |
| `failedInBlock` | Failed in a best or finalized block. |
| `refused`, `unverified`, `unknown` | Count of each status. |
| `inPool`, `lost` | Count of each. `null` if the pool or chain is unreadable. |
| `onChain` | Finalized blocks walked; `missed`, `onlyOnFork`, `moved` against the tracker. |
| `nodePool` | The node's mempool and ready counts after the run. |
| `finalized` | Last block with a run transaction, the finalized block at the wait's end, and whether it finalized in time. |
| `state` | Chain-state check of 100 included transactions: `checked`, `missing`, `detail`. Only with a state check. |
| `note` | Why something is missing. |

## Stop rules and thresholds

Breaking-point rules record a result, and the load goes on. The other rules end the load.

| Rule: trigger | Kind | Default | Plan attribute |
| --- | --- | --- | --- |
| **Included**: included share below the minimum | breaking point | 90% | `min-included-ratio` |
| **Latency**: p95 send to best block above the maximum | breaking point | 10 s | `max-p95-latency-ms` |
| **Refused**: refused share above the maximum | breaking point | 1% | `max-refused-ratio` |
| **Pool refuses**: refused share above the limit, this step or the last | graceful | 10% | `pool-refuses-ratio` |
| **Pool intake**: p95 submit reply above the limit, or one submit waited that long, or node stopped reading submits | graceful | 5 s | `max-submit-reply-ms` |
| **Slow blocks**: mean block gap above factor times `block-interval-seconds` | hard | 2.0 | `max-block-gap-factor` |
| **Stall**: no new best block for this long | hard | 30 s | `stall-ms` |
| **Finality stall**: no new finalized block for this long | hard | 60 s | `finality-stall-ms` |
| **Node down**: node closed an RPC connection | hard | none | none |
| **Rate cap**: every step ran | none | none | none |
| **Budget used up**: prepared transactions ran out, or a short step had nothing to send | none | none | none |
| **Generator limit**: sent share of target rate below minimum, or the load tool paused over 5 s between sends | none | 95%; 5 s between sends (fixed) | `min-send-ratio` |
| **Smoke error**: monitor problem, smoke mode only | none | none | none |

Ratios accept values above 0 up to 1. Factors and durations must be above 0. Set them in the plan's [`<thresholds>`](plans.md#thresholds).

## Recovery

After the load, the run sends one probe per block, with no load on the chain.

| Setting | Default | Meaning |
| --- | --- | --- |
| `recovery-seconds` | 900 | Probe budget after the load stops. |
| `probes-in-a-row` | 3 | Consecutive in-time probes needed. |
| `recovered-block-gap-factor` | 1.5 | Largest mean block gap, in block intervals, that counts as recovered. |
| `finality-wait-ms` | 180000 | Longest wait to finalize past the last run transaction's block. |

A probe is in time if it lands within the larger of twice the slowest baseline probe and twice the block interval. The chain has recovered at the first run of in-time probes that meets `probes-in-a-row`, if the blocks from that run on average stay within the factor. Probing stops at recovery or when the budget runs out. The block follower then drains for up to 30 seconds.

## Failure modes

| Class | Meaning | Recorded when |
| --- | --- | --- |
| `graceful` | The node refuses or delays transactions, and keeps producing blocks. | Load ended on pool refuses or pool intake. |
| `hard` | Block production or finality degrades for everyone. | Load ended on slow blocks, a stall, a finality stall or node down; or a transaction failed after inclusion. |
| `silent` | A transaction is gone with no error, or left no state. | A transaction is lost, or an included one left no state. |

## Checks

Checks run on the recorded series, in this order. A check whose metric the node does not export returns `no result`.

| Outcome: check | Judges | Status |
| --- | --- | --- |
| block production: why blocks end | Why each block ended: weight, size, deadline, or no more transactions. | `info` |
| block production: build time within the authoring deadline | Build time against 2 s. | `warn`: a block over 2.5 s, or p95 at 2.5 s. `fail`: over 5% of blocks above 2.5 s. |
| pvf: relay slots (level 1) | Slots missed above idle: not built, not backed, timed out. | `fail`: a window misses over one slot above idle. |
| pvf: no timed-out candidates or disputes | Timed-out candidates; relay disputes. | `fail` if any. |
| pvf: PVF time on validators (level 2, all parachains) | Validation time. | `warn`: a validation over 2 s, or an invalid result. |
| pvf: collation funnel | Collations expired after advertisement, fetch or backing; mean backing-to-inclusion time. | `warn`: any expired, or mean over 1.5 relay blocks. |
| pool: no silent loss | Accepted transactions in no block, without error. | `fail` if any. `no result` if the pool cannot be listed. |
| pool: the burst drains | Probes in time, backlog drained, no empty block while over 10 transactions waited. | `fail` unless all three hold. |
| pool: refusals are clean | Refusals by reason. | `fail`: no code. `warn`: run stopped at pool intake. |
| pool: pool work leaves time for blocks | Pool maintenance p95; validation backlog. | `fail`: p95 over half the block interval. `warn`: over 1000 waiting for validation. |
| run: monitors recorded everything | Monitor problems. | `warn` if any. |

Statuses: `pass` (no problem found), `warn` (a `WARN` verdict unless the verdict is `FAIL`), `fail` (a `FAIL` verdict), `info` (a number to read, not judged), `no result` (data missing, or a node restarted inside the window).

The limits are in `crates/checks/src/limits.rs`; they are placeholders until the load target and budgets are agreed. Plugin checks come after the built-in ones and count toward the verdict the same way. An `optional` plugin check with no result does not make the verdict `INCONCLUSIVE`.

## Verdict

One verdict is written as `- **Verdict:**` in `summary.md`. The first matching row decides.

| Order | Verdict | When |
| --- | --- | --- |
| 1 | `FAIL` | The stop rule has a class (`graceful` or `hard`); `failureModes` is not empty; there is a breaking point; or any check is `fail`. |
| 2 | `INCONCLUSIVE` | The stop rule is not `rate cap`, or a required check has `no result`. Checks are required unless marked `optional`. |
| 3 | `WARN` | Any check is `warn`. |
| 4 | `PASS` | Otherwise. |

Plans without `<load>` have no verdict. Neither `execution.json` nor `summary.json` has a verdict key.

## samples.jtl

One header row, then 13 columns.

| Column | Step rows | Transaction rows (load test) |
| --- | --- | --- |
| `timeStamp` | Event time, in milliseconds | `sentAt` |
| `elapsed` | `elapsedMs` of the step | `inclusionMs`, or `replyMs` without inclusion |
| `label` | `step:<step id>` | `load:step:<step index>` |
| `responseCode` | `STEP_OK` or `STEP_FAILED` | Transaction status |
| `responseMessage` | Step error, or empty | Transaction hash |
| `threadName` | `user:<n>:iteration:<n>`, or `null` outside a workflow | `load` |
| `success` | `true` if the step succeeded | `true` if finalized and not failed |
| `bytes`, `sentBytes` | `0` | `0` |
| `Latency` | `0` | `replyMs` |
| `Connect` | `0` | `0` |
| `allThreads`, `grpThreads` | `1` | `1` |

## events.jsonl

Each event has a `timestamp` in milliseconds and `version` `2`. Credentials are redacted.

| `event` | Fields | When |
| --- | --- | --- |
| `phase` | `phase`, and `id` for a workflow | Each phase start: `preflight`, `setup`, `workflow`, `teardown`. A load test adds, in order, `measurement`, `baseline`, `load`, `recovery`, `reconciliation` and `checks`. |
| `step-started` | `step`, `operation`, `user`, `iteration` | A step starts. |
| `step-progress` | `step`, `elapsedMs` | Every 5 seconds while a step runs. |
| `step-finished` | `step`, `elapsedMs`, `success`, `error`, `user`, `iteration` | A step ends. |
| `cleanup-failed` | `error` | A teardown step failed. |
| `completed` | `outcome` | The run ended, with the same outcome as `execution.json`. |

`--format json` adds `artifact-written`, `report` and `validation` lines, which are not written to `events.jsonl`.

## Exit codes

Each command's exit code, and what sets it, is in the [CLI reference](cli.md#exit-codes).

## Rebuilding a report

`polkameter report <run directory>` rebuilds the files from the raw data and prints `summary.md`. It needs `execution.json` and `summary.md`.

| Plan | Rewrites |
| --- | --- |
| Without `<load>` | `samples.jtl` and the plots |
| With `<load>` | Also the checks, `summary.json` and `summary.md` |

The result comes only from the files, so you can copy a run directory to another machine and report it there.
