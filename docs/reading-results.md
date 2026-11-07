# Read the results

A load test writes its answers to `summary.md` in the run directory. The examples on this page come from the remarks run in [Stress-test a local dev chain](dev-chain.md). It ramps from 100 to 600 transactions per second (tx/s), 30 seconds per rate step.

## Did it pass?

Look at the `Verdict:` line in `summary.md`, or `verdict.result` in `summary.json`. The verdict is the load test's result. The first rule that matches decides it:

- FAIL: the run recorded a failure stop, a failure mode, a breaking point or a failed check.
- INCONCLUSIVE: nothing failed, but the load ended early or a required check has no result.
- WARN: every rate step ran and a check warned.
- PASS: every rate step ran, and no check failed, warned or lacks a result.

PASS doesn't prove the chain's maximum capacity. It only says the chain met every limit at the rates you chose.

Here's the result from the remarks run:

```markdown
- **Verdict:** FAIL
- **Failure onset** (first failed load step): step 5 (600 tx/s): latency, p95 send -> best block 12.9 s
- **Max sustained:** 500 tx/s target (step 4), 500 tx/s included
- **Load stop:** rate cap: all 6 steps ran
```

Three separate things tell you how a run ended:

| Question | Where to look | Example |
| --- | --- | --- |
| Did the chain pass the test? | `Verdict:` in `summary.md`, `verdict.result` in `summary.json` | `FAIL` |
| Why did the load stop? | `Load stop:` in `summary.md`, `stop` in `summary.json` | `rate cap: all 6 steps ran` |
| Did the command finish without an error? | The exit code, and `state` in `execution.json` | `1`, `completed_with_failures` |

> **Warning:** Exit code `0` doesn't mean the test passed. By default, a run with a FAIL or INCONCLUSIVE verdict still exits `0`. Add `--fail-on-verdict` and `--fail-on-inconclusive` to make them exit `1`. [`run`](cli.md#run) explains the flags and the extra rules of smoke mode.

The outcome checks table at the end of `summary.md` gives each check a status: `pass`, `warn`, `fail`, `info` or `no result`. A check never passes on data it doesn't have. Its detail says what's missing. A check for nodes the plan doesn't monitor is optional and doesn't change the verdict. [Checks](results.md#checks) lists each check.

### What to do about INCONCLUSIVE

Read the stop reason and the checks with no result. Fix the cause before you run the test again. The `Load stop:` line and the outcome checks table show which cause applies:

- The load ended on budget used up. The prepared flood transactions ran out before the last rate step finished. Prepare more, or shorten or slow the rate steps.
- The load ended on generator limit. PolkaMeter sent less than 95% of the target rate, or the machine paused. Use a faster machine, lower the rate, or keep the machine awake.
- The load ended on smoke error. A smoke-mode run stopped at the first monitor problem. The `smoke:` lines on standard error name it.
- A required check has no result. Its detail says why. Usually a monitored node didn't answer its metrics scrape, a node restarted, or the plan's state check failed.

In CI, if you can't fix the cause yet, set [`fail-on: verdict`](ci.md#what-fails-the-job) so that only FAIL fails the job.

## How much load did the chain take?

The `Load steps` table has one row per rate step. This copy keeps the last three of six steps and a few columns:

```markdown
| step | target tx/s | duration s | transactions sent | sent/s | included/s | included % | p50 s | p95 s (limit 10.0) | ... | max ours/block | max ref time % | max proof % |
...
| 3 | 400 | 30.0 | 12000 | 400 | 400 | 100.0 | 3.9 | 6.6 | ... | 2409 | 78.3 | 5.4 |
| 4 | 500 | 30.0 | 15001 | 500 | 500 | 100.0 | 4.1 | 6.8 | ... | 3011 | 97.9 | 6.8 |
| 5 | 600 | 30.0 | 18001 | 600 | 600 | 100.0 | 6.2 | 12.9 | ... | 3075 | 100.0 | 6.9 |
```

The breaking point is the first rate step that fails an inclusion, latency or refusal limit. `summary.md` calls it the failure onset. Here it's step 5: its p95 was 12.9 s, above the 10 s limit. A breaking point doesn't stop the load.

The max sustained rate is the target rate of the step before the first breaking point. If no step breaks a limit, it's the last step's rate. If the first step breaks a limit, there's no value. Here it's 500 tx/s. It's not the chain's absolute capacity.

[Stop rules and thresholds](results.md#stop-rules-and-thresholds) lists every rule. The plan's [`<thresholds>`](plans.md#thresholds) sets the limits and lists the defaults. A dropped connection that PolkaMeter reconnects isn't a stop. [How a run works](how-it-works.md#why-a-dropped-connection-is-not-a-chain-failure) explains why.

The run directory also has plots in `plots/`: throughput over time, latency percentiles and failures by status.

## Which step does a number belong to?

A transaction counts in the rate step that sent it, even when a later block includes it. Its inclusion, its latency and its share of `included/s` stay with that step. So a step's `included/s` is its own included transactions divided by the step's length. It doesn't count what the step before left in the pool.

Blocks count in the step they arrive in. A block gap counts in each step it spans, for the part inside that step.

## How slow did it get?

The p50 and p95 columns measure the time from submission to inclusion in a best block. p50 is the median. p95 is the time within which about 95% of the included transactions reached a best block, so only the slowest 5% took longer. The breaking point checks p95 against its limit.

The `reply p95` column is how long the node took to answer a submission. In the remarks run it stayed at 0.0 s, even at 600 tx/s. So the node accepted the transactions quickly, but full blocks made them wait.

## Did transactions get lost?

Loss accounting gives each sent flood transaction one final status. Funding transactions and probes are separate. The loss check line in `summary.md` gives the totals:

```markdown
- **Loss check:** 63002 sent: 63002 finalized (0 failed after inclusion), 0 refused, 0 still ready in the pool, 0 seen in a block but not verified final, **0 lost**, 0 unknown. ...
```

Only `lost` counts as lost. A transaction is `lost` only when PolkaMeter could read both the finalized chain and the node's pool. Without that evidence it stays `unverified` or `unknown`. So check those two counts too before you call a run loss-free. Any lost transaction makes the verdict FAIL. [Loss accounting](results.md#loss-accounting) defines each status.

`lost.jsonl` lists the first 1000 lost transactions. An empty file means no confirmed loss, not proof that nothing was lost.

If the plan has a state check and it gives no result, the loss check line says `State check: no result`. Then the "no silent loss" check can't pass. Confirmed loss still fails it, and any failure keeps the verdict FAIL. Otherwise the missing result makes the verdict INCONCLUSIVE.

## Did the chain recover?

Before the load, the baseline sends a few probe transactions to measure latency on a quiet chain. After the load stops, PolkaMeter sends one probe per block. Recovery needs consecutive probes within the latency threshold and a mean block gap within the configured limit. By default, that's three probes in a row and a mean gap of at most 1.5 block intervals. The latency threshold is the larger of twice the slowest baseline probe and twice the block interval.

```markdown
- **Recovery:** back 1 s after the load stopped (3 probes in a row landed within 12.0 s). Backlog at stop 5712 txs, drained after 8 s, at 317.3 tx/s.
```

`Backlog at stop` counts transactions still waiting when the load stopped. `drained after` is how long the pool took to empty. If the chain doesn't recover within `recovery-seconds`, the run gives up. [Recovery](results.md#recovery) lists the settings.
