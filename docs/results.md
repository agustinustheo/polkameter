# Run directory

Each run stores its reports and recorded data in a new run directory. [Read the results](reading-results.md) explains how to interpret them. A plan with a `<load>` element is a load test. Other plans write fewer files.

## The run directory

Example plan, no load test (plot files trimmed):

```text
run-1792253315025-76883-1/
  events.jsonl
  execution.json
  junit.xml
  plan.json
  plots/                      throughput.svg, latency-percentiles.svg, failure-breakdown.svg
  plugins/example/stderr.log
  resolved-plan.json
  samples.jtl
  summary.md
```

The name is `run-<milliseconds>-<pid>-<sequence>`. The directory is created in `--output`. A run never reuses a directory.

## Files in every run

| File | Contents |
| --- | --- |
| `execution.json` | How the run ended. |
| `junit.xml` | Test report for CI. See [junit.xml](#junitxml). |
| `summary.md` | No load test: plan name, state, and the error or `All configured steps completed.` Load test: the full report. |
| `events.jsonl` | Every event, one JSON object per line. |
| `plan.json` | The plan as parsed. |
| `resolved-plan.json` | Plugin manifests and registry records, `<monitors>` settings, CLI version and BLAKE2 hash. |
| `samples.jtl` | One CSV row per step, and per transaction in a load test. See [samples.jtl](#samplesjtl). |
| `plots/*.svg` | Throughput, latency percentiles, and failures by status. |
| `plugins/<id>/stderr.log` | Standard error of each plugin that started. |

## Redaction

Redaction replaces known credential values and endpoint secrets with `[redacted]`. PolkaMeter redacts:

- the value of each credential the plan declares, read from the environment variable that [`polkameter plugin credential`](cli.md#plugin) maps it to;
- the user information, query values, path parameter values (`;name=value`) and fragment of each target endpoint and monitor URL.

An endpoint keeps its scheme, host, port, path and parameter names. For example, `wss://user:pass@rpc.example/ws?apikey=KEY` is written as `wss://[redacted]@rpc.example/ws?apikey=[redacted]`.

Redaction covers every file PolkaMeter writes in the run directory, including the load-test records. It also covers each plugin's `stderr.log`, the `--format json` events, and the messages on standard error. PolkaMeter also finds a secret in its JSON-escaped forms.

PolkaMeter doesn't redact:

- files that a plugin writes into the run directory, other than its `stderr.log`;
- a secret that a plugin encodes or transforms, for example as base64;
- a secret in a URL path, such as `https://rpc.example/v3/KEY`;
- a password with an unencoded `?`. Percent-encode it as `%3F`.

## Files in a load test

| File | Contents | Needs |
| --- | --- | --- |
| `calibration.json` | Configured and measured block interval. | |
| `summary.json` | The summary as data. | |
| `steps.jsonl` | Per rate step and lane: target rate, sent, included, refused, p50 and p95 latency, block gaps, node CPU and memory. See [Which step does a number belong to?](reading-results.md#which-step-does-a-number-belong-to) | |
| `load.jsonl` | The load tool's `polkameter_*` metrics. | |
| `blocks.jsonl` | Each best block, and the gap since the previous one. The gap is `null` for the first block after a reconnect. | |
| `transactions.jsonl` | Each sent flood transaction and its [status](#loss-accounting). | |
| `lost.jsonl` | The first 1000 lost transactions. `loss.lost` holds the full count. Empty if none. | |
| `plugin-checks.json` | Checks from evaluate steps. Empty if none. | |
| `resolved-targets.json` | Scrape targets resolved from the topology, and its hash. | `<monitors>` |
| `scrapes.jsonl` | Raw `/metrics` per node. `error` if a node did not answer. | `<monitors>` |
| `chain.jsonl` | The relay recorder's series for the observed parachain. | `<monitors>` |
| `node.jsonl` | Node CPU and memory during the load, and the process ID sampled. | A node on this machine. See [Node CPU and memory](how-it-works.md#node-cpu-and-memory). |
| `plugins/<id>/series.jsonl`, `metrics.json` | A plugin's series and metric declarations. | A plugin that records metrics |

## execution.json

PolkaMeter writes `execution.json` after teardown. When you stop a run, it writes a provisional file with the state `stopping` at once. It replaces that file when the run ends.

| Field | Meaning |
| --- | --- |
| `artifact_dir` | Absolute path of the run directory. |
| `exit_code` | The exit code of the `run` command. |
| `state` | How the run ended, as below. |
| `error` | Error text, redacted. `null` if none. |
| `teardown` | `completed`, `failed` (every teardown step ran, at least one failed) or `interrupted` (a forced stop ended it). Absent if teardown did not run. |
| `failed_phase` | Only when `state` is `failed`: the phase, such as `preflight`, `setup`, `measurement`, `teardown` or `report`. |
| `error_location` | Only when `error` points into the plan: `{"line": …, "column": …}`. |
| `exit_reason` | Only when `state` is `completed_with_failures`: why the run exits `1`, redacted. |
| `stop_requested_at` | Only in a provisional file: when the stop was requested, in milliseconds. |

| `state` | `exit_code` |
| --- | --- |
| `completed` | `0` |
| `completed_with_failures` | `1` |
| `stopped` | `130` after SIGINT, `143` after SIGTERM |
| `stopping` | `130` or `143`. Provisional. If it remains after the process ended, the process was killed. |
| `failed` | `3` before setup, `1` in setup or later |

[Exit codes](cli.md#exit-codes) lists the causes of each code.

## junit.xml

`junit.xml` is a test report that CI systems such as GitLab, Jenkins and GitHub test-report actions can display. Every run tries to write it after teardown, also when the run failed or was stopped. `--junit PATH` on `run` or `report` writes one more copy to `PATH`.

| Recorded result | JUnit test case |
| --- | --- |
| Plan execution, and each plan step | One case each. Success passes. A failure is `<failure>`. A step that never started is `<skipped>`. |
| Each load check | One case. `fail` is `<failure>`. `warn` passes, with the warning in the output. `no result` is `<skipped>`. |
| The load verdict | One case, `overall verdict`. FAIL is `<failure>`. INCONCLUSIVE is `<skipped>`. |

The JUnit report doesn't change the exit code.

## summary.md

Sections of a load test's `summary.md`:

| Section | Contents |
| --- | --- |
| `### <plan name>`, `#### Artifact under test`, `#### Stress method` | Title, what the transactions exercise, and the method. |
| `#### Result` | Verdict, Failure onset (`breakingPoint`), Max sustained (`maxSustained`), Load stop (`stop`), Recovery (`recovery`), Loss check (`loss`). |
| `#### Load steps` | One row per rate step, then one for recovery. |
| Collator metrics | Per rate step and recovery: node CPU and memory, block build time, pool work, why blocks ended. |
| Recovery probes | Probes sent after the load, and when each landed. |
| `### Outcome checks` | One row per check: outcome, check, status, detail. |

## summary.json

The measurement writes `summary.json` first, so plugin checks can read it. The report then adds the checks and the verdict, and writes `summary.md`. A plan without `<load>` writes no `summary.json` and has no verdict. `execution.json` has no verdict.

| Key | Holds |
| --- | --- |
| `schemaVersion` | Layout version, `1`. |
| `verdict` | `result`: `PASS`, `WARN`, `FAIL` or `INCONCLUSIVE`. |
| `stop` | `rule`, `detail`, `step`, `class`. |
| `breakingPoint` | `step`, `targetRate`, `measure` (`included`, `latency` or `refused`), `detail`. `null` if none. |
| `maxSustained` | `step`, `targetRate`, `includedPerS` of the step before the breaking point, or of the last step if there is no breaking point. `null` if the first step breaks a limit. |
| `loss` | Transaction counts. See [Loss accounting](#loss-accounting). |
| `checks` | `outcome`, `check`, `status`, `detail`, and `numbers` if any. `"optional": true` on optional checks. |

The SDK type [`Summary`](https://docs.rs/polkameter-plugin-sdk/latest/polkameter_plugin_sdk/observers/files/summary/struct.Summary.html) documents the other keys.

## Loss accounting

Loss accounting gives each sent flood transaction one final status. Funding transactions and probes are separate. Finality confirms that a block belongs to the chain's permanent history. Inclusion in a best block does not establish finality.

Each transaction gets the first status that holds:

| Status | Meaning |
| --- | --- |
| `finalized` | In a finalized block. Dispatch can still have failed. |
| `in_pool` | Still in the node's ready pool. |
| `refused` | The node rejected the submission. |
| `lost` | Accepted or in a best block, but absent from the finalized chain and the ready pool. PolkaMeter needs both sources to decide this. |
| `unverified` | In a best block, but PolkaMeter could not verify finality. |
| `unknown` | Not enough evidence for another status. |

Fields of the `loss` object in `summary.json`:

| Field | Meaning |
| --- | --- |
| `sent` | Distinct flood transactions sent. The statuses add up to it. |
| `lost` | Count of `lost`. `null` when pool or finalized-chain evidence is insufficient. |
| `inPool` | Count of `in_pool`. `null` when the ready pool could not be read. |
| `stateError` | Why the plan's state check gave no result. Present only then. |

If the state check gives no result, the "no silent loss" check cannot pass. Confirmed loss still fails it, and any failure keeps the verdict FAIL. Otherwise the verdict is INCONCLUSIVE. The SDK type [`Loss`](https://docs.rs/polkameter-plugin-sdk/latest/polkameter_plugin_sdk/observers/files/summary/struct.Loss.html) documents the other fields.

## Stop rules and thresholds

Breaking-point rules record a result, and the load continues. The other rules end the load. A graceful or hard stop is a failure. The plan's [`<thresholds>`](plans.md#thresholds) sets each limit and lists the defaults.

| Rule: trigger | Kind |
| --- | --- |
| Included: included share below the minimum | breaking point |
| Latency: p95 time from submission to best block above the maximum | breaking point |
| Refused: refused share above the maximum | breaking point |
| Pool refuses: refused share above the limit, in this step or the last | graceful |
| Pool intake: p95 submission reply above the limit, a submission waited that long, or the node stopped reading submissions | graceful |
| Slow blocks: mean block gap above the factor times `block-interval-seconds`. A step with no new block counts its whole length. | hard |
| Stall: no new best block for the limit, not counting reconnect time | hard |
| Finality stall: no new finalized block for the limit, not counting reconnect time | hard |
| Node down: the node stayed unreachable for 15 s, a connection dropped more than 5 times in 60 s, or a local node's process restarted | hard |
| Rate cap: every rate step ran | none |
| Budget used up: the prepared flood transactions ran out before the last rate step finished | none |
| Generator limit: sent share of the target rate below the minimum, or the load tool paused more than 5 s | none |
| Smoke error: a monitor problem, in smoke mode only | none |

A dropped RPC connection that reconnects is a problem, not a stop. See [Why a dropped connection is not a chain failure](how-it-works.md#why-a-dropped-connection-is-not-a-chain-failure).

## Recovery

After the flood stops, the run sends one probe transaction per block. A probe is in time if it lands within the larger of twice the slowest baseline probe and twice the block interval.

The chain has recovered when `probes-in-a-row` consecutive probes are in time and the mean block gap from then on is at most `recovered-block-gap-factor` block intervals. Probing continues until the recovery criteria are met and the backlog drains, unless the `recovery-seconds` budget or a node failure ends it. Then PolkaMeter waits up to `finality-wait-ms` for finality.

## Failure modes

| Class | Meaning | Recorded when |
| --- | --- | --- |
| `graceful` | The node refuses or delays transactions, and keeps producing blocks. | Load ended on pool refuses or pool intake. |
| `hard` | Block production or finality degrades for everyone. | Load ended on slow blocks, a stall, a finality stall or node down, or a transaction failed after inclusion. |
| `silent` | A transaction is gone with no error, or left no state. | A transaction is lost, or an included one left no state. |

## Checks

Checks run on the recorded data, in this order. A check whose metric the node does not export returns `no result`.

| Outcome: check | Measures | Status |
| --- | --- | --- |
| block production: why blocks end | Why each block ended: weight, size, deadline, or no more transactions. | `info`. `no result` if the collator recorded no block. |
| block production: build time within the authoring deadline | Build time against 2 s. | `warn`: a block over 2.5 s, or p95 at 2.5 s. `fail`: over 5% of blocks above 2.5 s. |
| pvf: relay slots (level 1) | Slots missed above idle: not built, not backed, timed out. | `fail`: a window misses more than one slot above idle. |
| pvf: no timed-out candidates or disputes | Timed-out candidates and relay disputes. | `fail` if any. |
| pvf: PVF time on validators (level 2, all parachains) | Validation time. | `warn`: a validation over 2 s, or an invalid result. |
| pvf: collation funnel | Expired collations, and mean backing-to-inclusion time. | `warn`: any expired, or mean over 1.5 relay blocks. |
| pool: no silent loss | Accepted transactions in no block, without error. | `fail`: confirmed loss. Otherwise `no result` when evidence or the requested state check is missing. |
| pool: the burst drains | Probes in time, backlog drained, no empty block while more than 10 transactions waited. | `fail` unless all three hold. |
| pool: refusals are clean | Refusals by error code. | `fail`: a refusal without an error code. `warn`: the run stopped on pool intake. |
| pool: pool work leaves time for blocks | Pool maintenance p95, and validation backlog. | `fail`: p95 over half the block interval. `warn`: over 1000 waiting for validation. |
| run: monitors recorded everything | Monitor problems, and time when blocks were not observed. | `warn` if any problem. `no result` if blocks went unobserved for longer than `stall-ms` in total. |

Statuses: `pass` (no problem found), `warn` (a WARN verdict only when no FAIL or INCONCLUSIVE rule applies), `fail` (a FAIL verdict), `info` (a number to read), `no result` (no data).

A check measures each window: a rate step, or recovery. A window has no data when a node's metrics are stale at its edges, a node restarted in it, or nothing was recorded. The detail names the window and the cause. A failure or warning in any window decides the status. Otherwise a window without data makes the check `no result`.

If the plan does not monitor what a check reads, the check is `no result` with a detail that starts `not applicable:`. Such a check is optional, so it does not make the verdict INCONCLUSIVE. Plugin checks come after the built-in ones and count toward the verdict the same way. An `optional` plugin check with no result does not make the verdict INCONCLUSIVE.

## samples.jtl

`samples.jtl` uses JMeter's CSV columns so JMeter report tools can read it. JMeter is a load-testing tool for websites. The file has one header row, then 13 columns.

| Column | Step rows | Transaction rows (load test) |
| --- | --- | --- |
| `timeStamp` | Start time, in milliseconds since the Unix epoch | Time of submission |
| `elapsed` | Step duration, in milliseconds | Time from submission to inclusion, or to the reply without inclusion |
| `label` | `step:<step id>` | `load:step:<step index>` |
| `responseCode` | `STEP_OK` or `STEP_FAILED` | Transaction status |
| `responseMessage` | Step error, or empty | Transaction hash |
| `threadName` | `steps` | `load` |
| `success` | `true` if the step succeeded | `true` if finalized and not failed |
| `Latency` | `0` | Reply time, in milliseconds |

The columns `bytes`, `sentBytes` and `Connect` are always `0`. `allThreads` and `grpThreads` are always `1`.

## events.jsonl

Each event has a `timestamp` in milliseconds and `version` `2`. Credentials are redacted.

| `event` | Fields | When |
| --- | --- | --- |
| `phase` | `phase` | Each phase start: `preflight`, `setup`, `teardown`. A load test adds, in order, `measurement`, `baseline`, `load`, `recovery`, `reconciliation` and `checks`. |
| `step-started` | `step`, `operation` | A step starts. |
| `step-progress` | `step`, `elapsedMs` | Every 5 seconds while a step runs. |
| `step-finished` | `step`, `elapsedMs`, `success`, `error` | A step ends. |
| `cleanup-failed` | `error`, `errors` | Teardown steps failed. `errors` lists every failure. |
| `plugin-restarted` | `plugin` | A plugin that crashed, timed out or was stopped was started again for a teardown step. |
| `warning` | `message` | Something did not stop the run: a reconnect to a different node, a plugin that could not be restarted, or a JUnit file that could not be written. |
| `completed` | `outcome` | The run ended, with the same outcome as `execution.json`. |

`--format json` adds `artifact-written`, `report` and `validation` lines. These are not written to `events.jsonl`.

## Rebuilding a report

`polkameter report <run directory>` rebuilds the reports from the recorded data and prints `summary.md`. It needs `execution.json` and `summary.md`.

| Plan | Rewrites |
| --- | --- |
| Without `<load>` | `junit.xml`, `samples.jtl` and the plots |
| With `<load>` | Also the checks, `summary.json` and `summary.md` |

The result comes only from the files. So you can copy a run directory to another machine and report it there.
