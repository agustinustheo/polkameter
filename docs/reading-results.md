# Read the results

A load test writes its answers to `summary.md` in the run directory. The examples on this page come from the remarks run in [Getting started](getting-started.md). It ramps from 100 to 600 transactions per second (tx/s), 30 seconds per rate step.

## Did it pass?

Look at the `Verdict:` line in `summary.md`, or `verdict.result` in `summary.json`. The verdict is the load test's result. The first rule that matches decides it:

- FAIL: the run recorded a failure stop, a failure mode, a breaking point or a failed check.
- INCONCLUSIVE: nothing failed, but the load ended early or a required check has no result.
- WARN: every rate step ran and a check warned.
- PASS: every rate step ran, and no check failed, warned or lacks a result.

PASS doesn't prove the chain's maximum capacity. It only says the chain met every limit at the rates you chose.

Three separate things tell you how a run ended:

| Question | Where to look | Example |
| --- | --- | --- |
| Did the chain pass the test? | `Verdict:` in `summary.md`, `verdict.result` in `summary.json` | `FAIL` |
| Why did the load stop? | `Load stop:` in `summary.md`, `stop` in `summary.json` | `rate cap: all 6 steps ran` |
| Did the command finish without an error? | The exit code, and `state` in `execution.json` | `1`, `completed_with_failures` |

> **Warning:** Exit code `0` doesn't always mean the test passed. A WARN verdict exits `0`, and so does every verdict with `--fail-on error`. [`run`](cli.md#run) explains the flag and the extra rules of smoke mode.

The outcome checks table at the end of `summary.md` gives each check a status: `pass` (no problem), `warn`, `fail`, `info` (a number to read) or `no result` (no data). A check never passes on data it doesn't have. Its detail says what's missing. A check for nodes the plan doesn't monitor is optional and doesn't change the verdict. [Outcome checks](results.md#outcome-checks) lists each check.

### What to do about INCONCLUSIVE

Read the stop reason and the checks with no result. Fix the cause before you run the test again. The `Load stop:` line and the outcome checks table show which cause applies:

- The load ended on budget used up. The prepared flood transactions ran out before the last rate step finished. Prepare more, or shorten or slow the rate steps.
- The load ended on generator limit. PolkaMeter sent less than 95% of the target rate, or the machine paused. Use a faster machine, lower the rate, or keep the machine awake.
- The load ended on smoke error. A smoke-mode run stopped at the first monitor problem. The `smoke:` lines on standard error name it.
- A required check has no result. Its detail says why. Usually a monitored node didn't answer its metrics scrape, a node restarted, or the plan's state check failed.

In CI, if you can't fix the cause yet, set [`fail-on: verdict`](ci.md#what-fails-the-job) so that only FAIL fails the job.

## How much load did the chain take?

The `Load steps` table has one row per rate step. This excerpt shows the last three of six steps:

```markdown
| step | target tx/s | duration s | transactions sent | sent/s | included/s | included % | p50 s | p95 s (limit 10.0) | ... | max ours/block | max ref time % | max proof % |
...
| 3 | 400 | 30.0 | 12000 | 400 | 400 | 100.0 | 3.9 | 6.6 | ... | 2409 | 78.3 | 5.4 |
| 4 | 500 | 30.0 | 15001 | 500 | 500 | 100.0 | 4.1 | 6.8 | ... | 3011 | 97.9 | 6.8 |
| 5 | 600 | 30.0 | 18001 | 600 | 600 | 100.0 | 6.2 | 12.9 | ... | 3075 | 100.0 | 6.9 |
```

The breaking point is the first rate step that fails an inclusion, latency or refusal limit. `summary.md` calls it the failure onset. Here it's step 5: its p95 was 12.9 s, above the 10 s limit. A breaking point doesn't stop the load.

The max sustained rate is the target rate of the step before the first breaking point. If no step breaks a limit, it's the last step's rate. If the first step breaks a limit, there's no value. Here it's 500 tx/s.

[Stop rules](#stop-rules) lists every rule. [`<thresholds>`](plans.md#thresholds) sets the limits.

## Which step does a number belong to?

A transaction counts in the rate step that sent it, even when a later block includes it. Its inclusion, its latency and its share of `included/s` stay with that step.

## How slow did it get?

The p50 and p95 columns measure the time from submission to inclusion in a best block. p50 is the median. p95 is the time within which about 95% of the included transactions reached a best block, so only the slowest 5% took longer. The breaking point checks p95 against its limit.

The `reply p95` column is how long the node took to answer a submission. In the remarks run it stayed at 0.0 s, even at 600 tx/s. So the node accepted the transactions quickly, but full blocks made them wait.

## Did transactions get lost?

The loss check line in `summary.md` gives the totals:

```markdown
- **Loss check:** 63002 sent: 63002 finalized (0 failed after inclusion), 0 refused, 0 still ready in the pool, 0 seen in a block but not verified final, **0 lost**, 0 unknown. ...
```

After the load, PolkaMeter gives each sent flood transaction the first status that holds. Funding transactions and probes aren't part of it.

| Status | Meaning |
| --- | --- |
| `finalized` | In a finalized block. Dispatch can still have failed. |
| `in_pool` | Still in the node's ready pool. |
| `refused` | The node rejected the submission. |
| `lost` | Accepted or in a best block, but absent from the finalized chain and the ready pool. PolkaMeter needs both sources to decide this. |
| `unverified` | In a best block, but PolkaMeter could not verify finality. |
| `unknown` | Not enough evidence for another status. |

Finality confirms that a block belongs to the chain's permanent history. Inclusion in a best block does not. Without reading the finalized chain, a transaction that a reorg moved to another block would look lost. So anything PolkaMeter couldn't check becomes `unknown` or `unverified`. A failed read never creates a loss. Check those two counts too before you call a run loss-free. Any lost transaction makes the verdict FAIL.

`lost.jsonl` lists the first 1000 lost transactions. An empty file means no confirmed loss, not proof that nothing was lost.

The plan's state check reads a sample of the finalized transactions and checks that they changed state. If it gives no result, the loss check line says `State check: **no result** (<reason>)`, and the "no silent loss" check cannot pass. Confirmed loss still fails it, and any failure keeps the verdict FAIL. Otherwise the verdict is INCONCLUSIVE.

## Did the chain recover?

```markdown
- **Recovery:** back 1 s after the load stopped (3 probes in a row landed within 12.0 s). Backlog at stop 5712 txs, drained after 8 s, at 317.3 tx/s.
```

After the flood stops, the run sends one probe transaction per block. A probe is in time if it lands within the larger of twice the slowest baseline probe and twice the block interval.

The chain has recovered when `probes-in-a-row` consecutive probes are in time and the mean block gap from then on is at most `recovered-block-gap-factor` block intervals. Probing continues until the recovery criteria are met and the backlog drains, unless the `recovery-seconds` budget or a node failure ends it. Then PolkaMeter waits up to `finality-wait-ms` for finality.

`Backlog at stop` counts transactions still waiting when the load stopped. `drained after` is how long the pool took to empty.

## Stop rules

Breaking-point rules record a result, and the load continues. The other rules end the load. The plan's [`<thresholds>`](plans.md#thresholds) sets each limit and lists the defaults.

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

A graceful stop means the node refuses or delays transactions but keeps producing blocks. A hard stop means block production or finality got worse for everyone. A transaction that failed after inclusion also counts as hard. A silent failure is a lost transaction, or an included one that left no state.

A dropped connection that reconnects is not a stop. See [blocks: RPC connection lost](troubleshooting.md#blocks-rpc-connection-lost--reconnected-after-seconds-s-nothing-observed-meanwhile).
