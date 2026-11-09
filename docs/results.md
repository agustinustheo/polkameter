# Run directory

Each run writes a new directory in `--output`, named `run-<milliseconds>-<pid>-<sequence>`. A run never reuses a directory. A plan with a `<load>` element is a load test. Other plans write fewer files.

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
| `plugins/<id>/stderr.log` | Standard error of each plugin that started. |

## Redaction

Redaction replaces known credential values and endpoint secrets with `[redacted]`. PolkaMeter redacts:

- the value of each credential the plan declares, read from the environment variable that [`polkameter plugin credential`](cli.md#plugin) maps it to;
- the user information, query values, path parameter values (`;name=value`) and fragment of each target endpoint and monitor URL.

An endpoint keeps its scheme, host, port, path and parameter names. For example, `wss://user:pass@rpc.example/ws?apikey=KEY` is written as `wss://[redacted]@rpc.example/ws?apikey=[redacted]`.

Redaction covers every file PolkaMeter writes in the run directory, including the load-test records. It also covers each plugin's `stderr.log` and the messages on standard error. PolkaMeter also finds a secret in its JSON-escaped forms.

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
| `steps.jsonl` | Per rate step: target rate, sent, included, refused, p50 and p95 latency, block gaps, node CPU and memory. See [Which step does a number belong to?](reading-results.md#which-step-does-a-number-belong-to) | |
| `load.jsonl` | The load tool's `polkameter_*` metrics. | |
| `blocks.jsonl` | Each best block, and the gap since the previous one. The gap is `null` for the first block after a reconnect. | |
| `transactions.jsonl` | Each sent flood transaction and its [status](reading-results.md#did-transactions-get-lost). | |
| `lost.jsonl` | The first 1000 lost transactions. `loss.lost` holds the full count. Empty if none. | |
| `plugin-checks.json` | Checks from evaluate steps. Empty if none. | |
| `resolved-targets.json` | Scrape targets resolved from the topology, and its hash. | `<monitors>` |
| `scrapes.jsonl` | Raw `/metrics` per node. `error` if a node did not answer. | `<monitors>` |
| `chain.jsonl` | The relay recorder's series for the observed parachain. | `<monitors>` |
| `node.jsonl` | Node CPU and memory during the load, and the process ID sampled. | A node on this machine. See [node: … no CPU or memory recorded](troubleshooting.md#node--no-cpu-or-memory-recorded). |
| `plugins/<id>/series.jsonl`, `metrics.json` | A plugin's series and metric declarations. | A plugin that records metrics |

## execution.json

PolkaMeter writes `execution.json` after teardown. When you stop a run, it writes a provisional file with the state `stopping` at once. It replaces that file when the run ends.

| Field | Meaning |
| --- | --- |
| `artifact_dir` | Absolute path of the run directory. |
| `exit_code` | The exit code of the `run` command. |
| `state` | How the run ended. |
| `error` | Error text, redacted. `null` if none. |
| `teardown` | `completed`, `failed` (every teardown step ran, at least one failed) or `interrupted` (a forced stop ended it). Absent if teardown did not run. |
| `failed_phase` | Only when `state` is `failed`: the phase, such as `preflight`, `setup`, `measurement`, `teardown` or `report`. |
| `error_location` | Only when `error` points into the plan: `{"line": …, "column": …}`. |
| `exit_reason` | Only when `state` is `completed_with_failures`: why the run exits `1`, redacted. |
| `stop_requested_at` | Only in a provisional file: when the stop was requested, in milliseconds. |

`state` is `completed`, `completed_with_failures`, `stopped`, `stopping` (provisional) or `failed`. [Exit codes](cli.md#exit-codes) gives each code.


## junit.xml

`junit.xml` is a test report that CI systems such as GitLab, Jenkins and GitHub test-report actions can display. Every run tries to write it after teardown, also when the run failed or was stopped.

| Recorded result | JUnit test case |
| --- | --- |
| Plan execution, and each plan step | One case each. Success passes. A failure is `<failure>`. A step that never started is `<skipped>`. |
| Each load check | One case. `fail` is `<failure>`. `warn` passes, with the warning in the output. `no result` is `<skipped>`. |
| The load verdict | One case, `overall verdict`. FAIL is `<failure>`. INCONCLUSIVE is `<skipped>`. |

The JUnit report doesn't change the exit code.

## summary.json

The measurement writes `summary.json` first, so plugin checks can read it. The report then adds the checks and the verdict, and writes `summary.md`. A plan without `<load>` writes no `summary.json` and has no verdict. `execution.json` has no verdict.

| Key | Holds |
| --- | --- |
| `schemaVersion` | Layout version, `1`. |
| `verdict` | `result`: `PASS`, `WARN`, `FAIL` or `INCONCLUSIVE`. |
| `stop` | `rule`, `detail`, `step`, `class`. |
| `breakingPoint` | `step`, `targetRate`, `measure` (`included`, `latency` or `refused`), `detail`. `null` if none. |
| `maxSustained` | `step`, `targetRate`, `includedPerS` of the step before the breaking point, or of the last step if there is no breaking point. `null` if the first step breaks a limit. |
| `loss` | Transaction counts. See [Did transactions get lost?](reading-results.md#did-transactions-get-lost) |
| `checks` | `outcome`, `check`, `status`, `detail`, and `numbers` if any. `"optional": true` on optional checks. |

The SDK type [`Summary`](https://docs.rs/polkameter-plugin-sdk/latest/polkameter_plugin_sdk/observers/files/summary/struct.Summary.html) documents the other keys.

## Outcome checks

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

A check measures each window: a rate step, or recovery. A window has no data when a node's metrics are stale at its edges, a node restarted in it, or nothing was recorded. The detail names the window and the cause. A failure or warning in any window decides the status. Otherwise a window without data makes the check `no result`.

If the plan does not monitor what a check reads, the check is `no result` with a detail that starts `not applicable:`. Plugin checks come after the built-in ones and count toward the verdict the same way.

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

Each event has a `timestamp` in milliseconds and `version` `2`. PolkaMeter redacts credentials in every event.

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

## Rebuilding a report

`polkameter report <run directory>` rebuilds the reports from the recorded data and prints `summary.md`. It needs `execution.json` and `summary.md`.

| Plan | Rewrites |
| --- | --- |
| Without `<load>` | `junit.xml` and `samples.jtl` |
| With `<load>` | Also the checks, `summary.json` and `summary.md` |

The result comes only from the files. So you can copy a run directory to another machine and report it there.
