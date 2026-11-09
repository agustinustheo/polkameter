# Run directory

Each run writes a new directory in `--output`, named `run-<milliseconds>-<pid>-<sequence>`. A run never reuses a directory. A [load test](plans.md#load), a plan with `<load>`, writes more files than other plans.

## Files in every run

| File | Contents |
| --- | --- |
| `execution.json` | How the run ended. |
| `junit.xml` | [Test report for CI](#junitxml). |
| `summary.md` | No load test: plan name, state, and the error or `All configured steps completed.` Load test: the full report. |
| `events.jsonl` | Every event, one JSON object per line. |
| `plan.json` | The plan as parsed. |
| `resolved-plan.json` | Plugin manifests and registry records, `<monitors>` settings, and the CLI's version and BLAKE2 hash. |
| `samples.jtl` | [One CSV row per plan step](#samplesjtl), and per transaction in a load test. |
| `plugins/<id>/stderr.log` | Standard error of each plugin that started. |

## Redaction

Redaction replaces known credential values and endpoint secrets with `[redacted]`. PolkaMeter redacts:

- the value of each credential the plan declares, read from the environment variable that [`polkameter plugin credential`](cli.md#plugin) maps it to;
- in each target endpoint and monitor URL: the user information (`user:pass@`), the query values, the path parameter values (`;name=value`) and the fragment (`#…`).

An endpoint keeps its scheme, host, port, path and parameter names. For example, `wss://user:pass@rpc.example/ws?apikey=KEY` is written as `wss://[redacted]@rpc.example/ws?apikey=[redacted]`.

Redaction covers every file PolkaMeter writes in the run directory, including the load-test records. It also covers each plugin's `stderr.log` and the messages on standard error. PolkaMeter also finds a secret in its JSON-escaped forms.

PolkaMeter doesn't redact:

- files that a plugin writes into the run directory, other than its `stderr.log`;
- a secret that a plugin encodes or transforms, for example as base64;
- a secret in a URL path, such as `https://rpc.example/v3/KEY`;
- a password with an unencoded `?`. Percent-encode it as `%3F`.

## Files in a load test

| File | Contents |
| --- | --- |
| `calibration.json` | Configured and measured block interval. |
| `summary.json` | The summary as data. |
| `steps.jsonl` | Per rate step: target rate, sent, included, refused, p50 and p95 latency, block gaps, node CPU and memory. |
| `load.jsonl` | PolkaMeter's own `polkameter_*` metrics. |
| `blocks.jsonl` | Each best block, and the gap since the previous one. The gap is `null` for the first block after a reconnect. |
| `transactions.jsonl` | Each sent flood transaction and its [status](reading-results.md#did-transactions-get-lost). |
| `lost.jsonl` | The first 1000 lost transactions. `loss.lost` holds the full count. |
| `plugin-checks.json` | Checks from evaluate steps. |
| `node.jsonl` | [Node CPU and memory](troubleshooting.md#node--no-cpu-or-memory-recorded) during the load, for a node on this machine. |
| `resolved-targets.json`, `scrapes.jsonl`, `chain.jsonl` | With `<monitors>`: each node's metrics URL, the raw metrics read from it, and the relay chain's record of the parachain. |
| `plugins/<id>/series.jsonl`, `metrics.json` | Extra data a plugin recorded. |

## execution.json

PolkaMeter writes `execution.json` after teardown. When you stop a run, it writes a temporary one with the state `stopping` at once. It replaces that file when the run ends.

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

CI systems such as GitLab, Jenkins and GitHub test-report actions can display `junit.xml`. Every run tries to write it after teardown, also when the run failed or was stopped.

| Recorded result | JUnit test case |
| --- | --- |
| Plan execution, and each plan step | One case each. Success passes. A failure is `<failure>`. A step that never started is `<skipped>`. |
| Each load check | One case. `fail` is `<failure>`. `warn` passes, with the warning in the output. `no result` is `<skipped>`. |
| The load verdict | One case, `overall verdict`. FAIL is `<failure>`. INCONCLUSIVE is `<skipped>`. |

The JUnit report doesn't change the exit code.

## summary.json

A load test writes `summary.json` before the evaluate steps, so plugin checks can read it. The report then adds the checks and the verdict, and writes `summary.md`. A plan without `<load>` writes no `summary.json` and has no verdict.

| Key | Holds |
| --- | --- |
| `schemaVersion` | Layout version, `1`. |
| `verdict` | `result`: `PASS`, `WARN`, `FAIL` or `INCONCLUSIVE`. |
| `stop` | `rule`, `detail`, `step`, `class`. |
| `breakingPoint` | The failure onset: `step`, `targetRate`, `measure` (`included`, `latency` or `refused`), `detail`. `null` if none. |
| `maxSustained` | `step`, `targetRate`, `includedPerS` of the [max sustained](reading-results.md#how-much-load-did-the-chain-take) rate step. `null` if the first rate step fails a limit. |
| `loss` | Transaction counts. See [Did transactions get lost?](reading-results.md#did-transactions-get-lost) |
| `checks` | `outcome`, `check`, `status`, `detail`, and `numbers` if any. `"optional": true` on optional checks. |

The plugin SDK's [`Summary`](https://docs.rs/polkameter-plugin-sdk/latest/polkameter_plugin_sdk/observers/summary/struct.Summary.html) type documents the other keys.

## Outcome checks

Checks run on the recorded data, in this order. The `block production`, `pvf` and `pool work leaves time for blocks` checks read node metrics, so they need `<monitors>`. PVF is the parachain validation function that relay chain validators run.

| Outcome: check | Measures | Status |
| --- | --- | --- |
| block production: why blocks end | Why each block ended: weight, size, deadline, or no more transactions. | `info`. `no result` if the collator recorded no block. |
| block production: build time within the authoring deadline | Build time against 2 s. | `warn`: a block over 2.5 s, or p95 at 2.5 s. `fail`: over 5% of blocks above 2.5 s. |
| pvf: relay slots (level 1) | Relay chain slots the parachain missed, beyond those it misses when idle. | `fail`: a window misses more than one slot above idle. |
| pvf: no timed-out candidates or disputes | Timed-out candidates and relay disputes. | `fail` if any. |
| pvf: PVF time on validators (level 2, all parachains) | Validation time. | `warn`: a validation over 2 s, or an invalid result. |
| pvf: collation funnel | Expired parachain block candidates, and mean backing-to-inclusion time. | `warn`: any expired, or mean over 1.5 relay blocks. |
| pool: no silent loss | Accepted transactions in no block, without error. | `fail`: confirmed loss. Otherwise `no result` when evidence or the requested state check is missing. |
| pool: the burst drains | Probes in time, backlog drained, no empty block while more than 10 transactions waited. | `fail` unless all three hold. |
| pool: refusals are clean | Refusals by error code. | `fail`: a refusal without an error code. `warn`: the run stopped on pool intake. |
| pool: pool work leaves time for blocks | Pool maintenance p95, and validation backlog. | `fail`: p95 over half the block interval. `warn`: over 1000 waiting for validation. |
| run: monitors recorded everything | Monitor problems, and time when blocks were not observed. | `warn` if any problem. `no result` if blocks went unobserved for longer than `stall-ms` in total. |

A check measures each window. A window is one rate step, or the recovery. A failure or warning in any window sets the status. Otherwise, a window without data makes the check `no result`. A node restart can leave a window without data. The detail names the window and the cause.

Plugin checks come after the built-in ones and count toward the verdict the same way.

## samples.jtl

`samples.jtl` uses the CSV columns of JMeter, a common load-testing tool, so JMeter's report tools can chart a run. The file has one header row, then 13 columns.

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

It rewrites `junit.xml` and `samples.jtl`, and in a load test also the checks, `summary.json` and `summary.md`.

`report` reads only the run directory, so you can copy it to another machine and run `report` there.
