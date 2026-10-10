# Results and verdicts

A run writes a directory of raw files and two summaries. This page explains what each file holds, how the verdict is reached, and what each stop rule and check measures.

## The run directory

Each run creates a directory named `run-<milliseconds>-<pid>-<sequence>` inside the output directory: the `--output` of `polkameter run`, the `--output-root` of a remote agent, or the desktop app's `runs` folder. Which files appear depends on the plan. A plan without a `<load>` produces only the first group.

| File | Written by | Contents |
| --- | --- | --- |
| `plan.json` | engine | The plan as parsed |
| `resolved-plan.json` | engine | Plugin manifests and checksums, the topology, and the CLI's engine version and hash |
| `resolved-targets.json` | engine | With monitors: the scrape targets resolved from the topology |
| `calibration.json` | engine | With a load: the configured and measured block interval, and the 25% tolerance |
| `events.jsonl` | engine | Every event of the run, one JSON object per line, each with `version` 2 and a timestamp |
| `execution.json` | engine | The outcome: `state`, `exit_code`, `error` and the artifact directory |
| `samples.jtl` | engine | JMeter CSV: one row per finished step, and with a load one row per transaction |
| `summary.md` | report | The report: verdict, stress method, result, load steps and checks |
| `summary.json` | report | The same summary as data, including the checks and the rules used |
| `plugin-checks.json` | engine | Checks returned by evaluate steps |
| `plots/*.svg` | engine (run and report) | `throughput.svg`, `latency-percentiles.svg` and `failure-breakdown.svg` |
| `steps.jsonl` | measurement | One record per step and lane: target rate, sent, included, refused, p50 and p95 latency, and the node's peak CPU and memory |
| `transactions.jsonl` | measurement | One record per submitted transaction, with its status and block |
| `lost.jsonl` | measurement | One record per lost transaction. The first 1000 are listed one by one, the rest only counted |
| `load.jsonl` | load tool | The load tool's own series: the `polkameter_*` counters and gauges |
| `blocks.jsonl` | load tool | One record per best block the tool saw, with the gap since the previous one |
| `chain.jsonl` | monitors | The relay recorder's series for the observed parachain |
| `scrapes.jsonl` | monitors | Raw `/metrics` scrapes of every node. A node that did not answer has an `error` |
| `node.jsonl` | monitors | The node's CPU and memory, sampled while it runs |
| `plugins/<id>/` | plugins | `stderr.log`, and any `series.jsonl` and `metrics.json` a plugin observer wrote |

`summary.json` and `summary.md` are written twice: once when the measurement ends, so plugin checks can read them, and again when the report adds the checks. `polkameter report` writes them again from the files alone.

## The verdict

Each measured run ends with one of four verdicts, shown as **Verdict** in `summary.md`. The first matching rule decides.

1. **FAIL** if any of these is true: the ramp ended on a graceful or hard failure; a failure mode was recorded (a lost transaction, a transaction that failed after inclusion, or an included transaction that left no state); a breaking point was found; or any check failed.
2. **INCONCLUSIVE** if the ramp ended on something other than rate cap, which means budget used up, generator limit or smoke error. The chain was not pushed to its limit, so the run tells you nothing. Also INCONCLUSIVE if the ramp reached rate cap but a required check has no result.
3. **WARN** if the ramp reached rate cap and some check warned.
4. **PASS** otherwise.

Individual checks use five statuses: `pass`, `warn`, `fail`, `info` (a number to read, not judged) and `no result` (the data is missing, or a node restarted inside the window).

## Stop rules and thresholds

A ramp moves through its steps until a stop rule fires. Response measures are checked on every step. The first one that a step violates is the **breaking point**, and the ramp continues. Failures end the ramp.

| Rule | Class | Fires when | Default | Threshold attribute |
| --- | --- | --- | --- | --- |
| Breaking point: included | none | Share of a step's transactions included is below the minimum | 90% | `min-included-ratio` |
| Breaking point: latency | none | p95 time from send to a best block is above the maximum | 10 s | `max-p95-latency-ms` |
| Breaking point: refused | none | Share of a step's submissions refused is above the maximum | 1% | `max-refused-ratio` |
| Pool refuses | graceful | Share refused in this step or the one before is above the limit | 10% | `pool-refuses-ratio` |
| Pool intake | graceful | p95 submit reply is above the limit, or a submit waited that long, or the node stopped reading submissions | 5 s | `max-submit-reply-ms` |
| Slow blocks | hard | Mean block gap is more than the factor times the block interval measured at the start | 2.0 | `max-block-gap-factor` |
| Stall | hard | No new best block for this long | 30 s | `stall-ms` |
| Finality stall | hard | No new finalized block for this long | 60 s | `finality-stall-ms` |
| Node down | hard | The node closed an RPC connection | none | none |
| Rate cap | none | All steps ran. The ramp finished | none | none |
| Budget used up | none | The prepared transactions ran out | none | none |
| Generator limit | none | The load tool could not send at the target rate | 95% | `min-send-ratio` |
| Smoke error | none | A monitor problem, in smoke mode only | none | none |

The graceful and hard classes end the ramp as failures. Stops with class "none" end it without a failure, and the verdict decides what they mean.

The ratio thresholds accept values above 0 and at most 1. The factors and durations must be above 0. The effective values are in `summary.json` under `rules`. Set them in the plan's [`<thresholds>`](plans.md#thresholds).

Two fields in `summary.json` answer the main questions:

- `breakingPoint`: the first step that violated a response measure, with its target rate, the measure (`included`, `latency` or `refused`) and the detail.
- `maxSustained`: the step before the breaking point, with its target rate and the transactions per second the chain included. With no breaking point, it is the last step.

`stop` records the rule that ended the load, the step, and its class.

## Recovery

After the load, the run sends one probe per block with no load on the chain, until the probes land in time again. A probe is in time when it is included within the recovery threshold. The threshold is the larger of twice the slowest baseline probe and twice the block interval. The chain has recovered at the first of `probes-in-a-row` consecutive in-time probes, provided the blocks from that probe on average no more than `recovered-block-gap-factor` times the block interval apart.

The run then drains the block follower for up to 30 seconds. `summary.json` records `recovery.recovered`, `recovery.seconds`, the backlog at the stop and at the end, the drain time, and the node's peak CPU and memory during recovery.

## Loss accounting

After recovery the run waits for the chain to finalize past the last block that holds one of its transactions, up to `finality-wait-ms`. It then classifies every transaction. Each one gets exactly one status in `transactions.jsonl`.

| Status | Meaning |
| --- | --- |
| `finalized` | Included in a finalized block. `failedInFinalizedBlock` says whether its dispatch failed |
| `in_pool` | Accepted, and still ready in the node's pool |
| `refused` | The node refused it |
| `lost` | Accepted or seen in a best block, but in no finalized block, not refused, and not ready in the pool. Only decided once the finalized chain and the pool were read |
| `unverified` | Seen in a best block, but finality was not verified: the wait timed out, or the finalized chain could not be walked |
| `unknown` | Not enough evidence: no reply and no inclusion, or the pool or the finalized chain could not be read |

The statuses partition `sent`. `unknown` and `unverified` are never counted as `lost`. The `loss` object in `summary.json` holds the totals: `sent`, `accepted`, `included` (finalized), `failedInBlock` (failed in a best or finalized block), `refused`, `inPool`, `lost`, `unverified` and `unknown`. A `null` `inPool` or `lost` was not counted, and its transactions fall under the other statuses. It also holds the counts read from the finalized chain (`onChain`), and the node's own mempool and ready-pool counts (`nodePool`).

Lost transactions produce one failure mode of class `silent`, with their count: they were accepted and no error came back. Transactions that failed after inclusion produce one failure mode of class `hard`.

## Failure modes

`failureModes` in `summary.json` lists the failures of a run, each with a class.

| Class | Meaning |
| --- | --- |
| `graceful` | The node refuses or delays transactions and keeps producing blocks |
| `hard` | Block production or finality degrades for everyone |
| `silent` | A transaction is gone with no error, or landed and left no state |

## Outcome checks

The checks run on the recorded series. Each belongs to an outcome. The `summary.md` table lists them in this order. Checks that need a metric the node does not export return `no result`.

| Outcome | Check | What it judges | Status |
| --- | --- | --- | --- |
| block production | why blocks end | Why the collator ended each block in each window (weight, size, deadline or no more transactions) | `info` |
| block production | build time within the authoring deadline | Block build time against the 2 s authoring deadline | `warn` when a window has a block over 2.5 s or a p95 at 2.5 s. `fail` when more than 5% of its blocks took over 2.5 s |
| pvf | relay slots (level 1) | Slots offered to the observed parachain that it missed, above its idle rate, split into not built, not backed and timed out | `fail` when a window misses more than one slot above idle |
| pvf | no timed-out candidates or disputes | Candidates of the parachain that timed out, and disputes on the relay | `fail` if any |
| pvf | PVF time on validators (level 2) | Validation execution time on the validators, for all parachains | `warn` when a window has a validation over 2 s, or an invalid result |
| pvf | collation funnel | Collations that expired after advertisement, fetch, or backing, and the mean time from backing to inclusion | `warn` if any expired, or the mean is over 1.5 relay blocks |
| pool | no silent loss | Transactions accepted, in no block, and without an error | `fail` if any. `no result` if the pool could not be listed |
| pool | the burst drains | Whether probes came back in time, the backlog drained, and no block ended empty while more than 10 of the run's transactions waited | `fail` unless all three hold |
| pool | refusals are clean | Refusals by reason | `fail` on refusals with no code. `warn` when the run stopped at pool intake |
| pool | pool work leaves time for blocks | Pool maintenance p95 against half the block interval, and the validation backlog | `fail` when the p95 is over half the block interval. `warn` when more than 1000 transactions wait for validation |
| run | monitors recorded everything | Whether any monitor reported a problem | `warn` if any did |

The limits are in `crates/checks/src/limits.rs`. They are placeholders until the load target and budgets are agreed.

Plugins add checks through evaluate steps. Their results are listed after the built-in ones and count toward the verdict. [Plugins](plugins.md#plugin-observers-and-checks) describes them.

## Exit codes

`polkameter run` exits with the outcome's exit code. The table covers the CLI commands. [CLI](cli.md) lists every command.

| Code | Meaning |
| --- | --- |
| `0` | The run completed. In stress mode this is true whatever the verdict. `report` also exits 0 on success |
| `1` | The run failed: an error in preflight, setup, a workflow or an evaluate step, or a failed assertion. Also a smoke-mode failure: a monitor problem, or a required check with no result. Also any other runtime error |
| `2` | The plan or the arguments are invalid, or a `plugin` subcommand failed |
| `3` | Preflight failed. Only `polkameter preflight` returns this |
| `130` | The run was stopped with Ctrl-C |

In stress mode, a run that finds a breaking point or a failed check exits `0` and records the result in the verdict. Smoke mode is for CI: it exits `1` when monitors fail or a required check has no result. A failed `core.assert-equal` step makes the run fail with exit `1` in both modes.

## Reading a result offline

`polkameter report <run directory>` rebuilds `samples.jtl`, the plots, the checks, `summary.json` and `summary.md` from the raw files. The result is the same as the original run's, so a run can be copied to another machine and checked there. The report needs `execution.json` and `summary.md` in the directory.
