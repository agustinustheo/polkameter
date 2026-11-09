# Read the results

A load test writes its results to `summary.md`, in the `run-*` directory that the `Run …` line names. The examples here come from the remarks run in [Getting started](getting-started.md#4-run-a-load-test). It ramps from 100 to 600 transactions per second (tx/s), 30 seconds per rate step.

## Did it pass?

Look at the `Verdict:` line in `summary.md`, or `verdict.result` in `summary.json`. The first rule that matches decides it:

- FAIL: any of these happened:
  - a rate step failed a limit (the [failure onset](#how-much-load-did-the-chain-take));
  - a graceful or hard [stop rule](#stop-rules) ended the load;
  - a transaction was [lost or failed](#did-transactions-get-lost);
  - a check failed.
- INCONCLUSIVE: nothing failed, but the result is incomplete. The load ended before its last rate step finished, or a required check has no result.
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

The outcome checks table at the end of `summary.md` gives each check a status: `pass`, `warn`, `fail`, `info` (a number to read) or `no result`. A check without data never passes. Its detail says what's missing. If the detail starts with `not applicable:`, the plan doesn't monitor what the check reads, and that `no result` doesn't change the verdict. [Outcome checks](results.md#outcome-checks) lists each check.

### What to do about INCONCLUSIVE

Read the `Load stop:` line and the checks with no result. Fix the cause before you run the test again:

- `budget used up: all N prepared transactions sent`. The flood ran out before the last rate step finished. Raise the prepare step's `count` above the total that the rate steps send (each rate times its seconds), or shorten or slow the rate steps.
- `generator limit`. PolkaMeter, the load generator, sent less than 95% of the target rate, or its machine paused. Use a faster machine, lower the rate, or keep the machine awake.
- `smoke error`. A smoke-mode run stopped at the first monitor problem. The `smoke:` lines that `run` prints name it.
- A required check has no result. Its detail says why. Usually a monitored node didn't answer when PolkaMeter read its metrics, a node restarted, or the plan's state check failed.

In CI, if you can't fix the cause yet, set [`fail-on: verdict`](ci.md#what-fails-the-job) so that only FAIL fails the job.

## How much load did the chain take?

The `Load steps` table has one row per rate step. This excerpt shows the last three of six:

```markdown
| step | target tx/s | duration s | transactions sent | sent/s | included/s | included % | p50 s | p95 s (limit 10.0) | ... |
...
| 3 | 400 | 30.0 | 12000 | 400 | 400 | 100.0 | 3.9 | 6.6 | ... |
| 4 | 500 | 30.0 | 15001 | 500 | 500 | 100.0 | 4.1 | 6.8 | ... |
| 5 | 600 | 30.0 | 18001 | 600 | 600 | 100.0 | 6.2 | 12.9 | ... |
```

The failure onset is the first rate step that fails a limit on inclusion, latency or refusals. Here it's step 5. Its p95 was 12.9 s, and the limit is 10 s. The load goes on until a [stop rule](#stop-rules) ends it. `summary.json` calls the failure onset `breakingPoint`. If no rate step fails, the `Failure onset` line says `no measure violated`. Then the run didn't find the chain's limit, so raise the rates.

The max sustained rate is the target rate of the step before the failure onset, here 500 tx/s. If no step fails, it's the last step's rate. If the first step fails, it's `none`.

A transaction counts in the rate step that sent it, even when a later block includes it.

## How slow did it get?

The p50 and p95 columns show the time from submission until a best block includes the transaction. A best block is the newest block the node knows. It may not be finalized yet. p50 is the median time. 95% of the included transactions took the p95 time or less.

The `reply p95` column, and the `step` lines that `run` prints, show how long the node took to answer a submission. In the remarks run it stayed at 0.0 s, even at 600 tx/s. So the node accepted the transactions quickly, but full blocks made them wait.

## Did transactions get lost?

The loss check line in `summary.md` gives the totals:

```markdown
- **Loss check:** 63002 sent: 63002 finalized (0 failed after inclusion), 0 refused, 0 still ready in the pool, 0 seen in a block but not verified final, **0 lost**, 0 unknown. ...
```

After the load, PolkaMeter gives each sent flood transaction the first status in this table that fits. Funding transactions and probes don't get one.

| Status | Meaning |
| --- | --- |
| `finalized` | In a finalized block. Its call can still have failed. |
| `in_pool` | In the node's ready pool: the transactions that wait for a block. |
| `refused` | The node rejected the submission. |
| `lost` | The node accepted it, or a best block had it. It's in no finalized block and not in the ready pool. PolkaMeter read both the finalized chain and the pool. |
| `unverified` | In a best block, but PolkaMeter could not verify finality. |
| `unknown` | Not enough evidence for another status. |

A finalized block is permanent. A best block isn't. In a reorg, the node switches to another fork, a competing branch of recent blocks, and its best blocks change. So when PolkaMeter can't check a transaction, its status is `unknown` or `unverified`, never `lost`. Check those two counts too before you call a run loss-free. A lost transaction, or one that failed after inclusion, makes the verdict FAIL.

`lost.jsonl` lists the first 1000 lost transactions. An empty file means no confirmed loss, not proof that nothing was lost.

The plan's state check reads a sample of finalized transactions and checks that each one took effect. If it has no result, the loss check line says `State check: **no result** (<reason>)`. Then the "no silent loss" check can't pass, and a run with no other failure is INCONCLUSIVE.

## Did the chain recover?

```markdown
- **Recovery:** back 1 s after the load stopped (3 probes in a row landed within 12.0 s). Backlog at stop 5712 txs, drained after 8 s, at 317.3 tx/s.
```

After the flood stops, the run sends one probe per block. A probe is in time when a block includes it within a time limit. The limit is twice the slowest baseline probe's time, or twice the block interval if that's longer. The chain has recovered when `probes-in-a-row` probes in a row are in time and blocks come at their normal pace again. [`<thresholds>`](plans.md#thresholds) and [`<load>`](plans.md#load) set these limits.

`Backlog at stop` counts transactions still waiting when the load stopped. `drained after` is how long the pool took to empty.

## Stop rules

Failure-onset rules only mark the failure onset, and the load goes on. The other rules end the load. The plan's [`<thresholds>`](plans.md#thresholds) sets each limit and lists the defaults.

| Rule: trigger | Kind |
| --- | --- |
| Included: included share below the minimum | failure onset |
| Latency: p95 time from submission to best block above the maximum | failure onset |
| Refused: refused share above the maximum | failure onset |
| Pool refuses: refused share above the limit, in this step or the last | graceful |
| Pool intake: the node answered or read submissions too slowly | graceful |
| Slow blocks: mean block gap above `max-block-gap-factor` times `block-interval-seconds` | hard |
| Stall: no new best block for the limit, not counting reconnect time | hard |
| Finality stall: no new finalized block for the limit, not counting reconnect time | hard |
| Node down: the node was unreachable for 15 s, reconnected too often, or restarted | hard |
| Rate cap: every rate step ran | end, no failure |
| Budget used up: the prepared flood ran out before the last rate step finished | end, no failure |
| Generator limit: PolkaMeter sent too little of the target rate, or paused more than 5 s | end, no failure |
| Smoke error: a monitor problem, in smoke mode only | end, no failure |

A graceful stop ends the load when the node refuses or delays transactions but still makes blocks. A hard stop ends the load when the node goes down, or blocks or finality slow down or stop for everyone.

A dropped connection that reconnects is not a stop. See [blocks: RPC connection lost](troubleshooting.md#blocks-rpc-connection-lost--reconnected-after-seconds-s-nothing-observed-meanwhile).
