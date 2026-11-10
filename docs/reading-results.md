# Read the results

A load test writes its results to `summary.md`, in the `run-*` directory that the `Run …` line names. The examples here come from the remarks run in [Getting started](getting-started.md#4-run-a-load-test). It ramps from 100 to 600 transactions per second (tx/s), 30 seconds per rate step.

## Did it pass?

Look at the `Verdict:` line in `summary.md`, or `verdict.result` in `summary.json`. The first rule that matches decides it:

- FAIL: any of these happened:
  - a rate step failed a limit (the [failure onset](#how-much-load-did-the-chain-take));
  - a graceful or hard [stop rule](#stop-rules) ended the load;
  - a transaction was [lost or failed](#did-transactions-get-lost);
  - a check failed.
- INCONCLUSIVE: nothing failed, but the result is incomplete. The load ended before its last rate step finished, the baseline has no result, or a required check has no result.
- WARN: every rate step ran and a check warned.
- PASS: every rate step ran, and no check failed, warned or lacks a result.

PASS doesn't prove the chain's maximum capacity. It only says the chain met every limit at the rates you chose.

Three separate things tell you how a run ended:

| Question | Where to look | Example |
| --- | --- | --- |
| Did the chain pass the test? | `Verdict:` in `summary.md`, `verdict.result` in `summary.json` | `FAIL` |
| Why did the load stop? | `Load stop:` in `summary.md`, `stop` in `summary.json` | `rate cap: all 6 steps ran` |
| Did the command finish without an error? | The exit code, and `state` in `execution.json` | `1`, `completed_with_failures` (`completed_inconclusive` for an INCONCLUSIVE verdict) |

> **Warning:** Exit code `0` doesn't always mean the test passed. A WARN verdict exits `0`, and so does every verdict with `--fail-on error`. [`run`](cli.md#run) explains the flag and the extra rules of smoke mode.

The outcome checks table at the end of `summary.md` gives each check a status: `pass`, `warn`, `fail`, `info` (a number to read) or `no result`. A check without data never passes. Its detail says what's missing. If the detail starts with `not applicable:`, the plan doesn't monitor what the check reads, and that `no result` doesn't change the verdict. [Outcome checks](results.md#outcome-checks) lists each check.

### What to do about INCONCLUSIVE

Read the `Load stop:` line and the checks with no result. Fix the cause before you run the test again:

- `budget used up: all N prepared transactions sent`. The flood ran out before the last rate step finished. Raise the prepare step's `count` above the total that the rate steps send (each rate times its seconds), or shorten or slow the rate steps.
- `generator limit`. PolkaMeter, the load generator, sent less than 95% of the target rate, or its machine paused. Or its connection to the node kept dropping (`connection unstable`), or nothing answered for 15 s (`node or network unreachable`). Use a faster machine, lower the rate, keep the machine awake, or fix the network or proxy in between.
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

The `reply p95` column, and the `step` lines that `run` prints, show how long the node took to answer a submission. Both times include the network between PolkaMeter and the node: see [Whose fault is it](#whose-fault-is-it). In the remarks run it stayed at 0.0 s, even at 600 tx/s. So the node accepted the transactions quickly, but full blocks made them wait.

## Did transactions get lost?

The loss check line in `summary.md` gives the totals:

```markdown
- **Loss check:** 63002 sent: 63002 finalized (0 failed after inclusion), 0 refused, 0 still ready in the pool, 0 seen in a block but not verified final, **0 lost**, 0 unknown. ...
```

After the load, PolkaMeter gives each sent flood transaction the first status in this table that fits. Funding transactions and probes don't get one.

| Status | Meaning |
| --- | --- |
| `finalized` | In a finalized block. Its call can still have failed. |
| `in_pool` | Reported by the node's ready-pool RPC. FUTURE transactions behind nonce gaps may be absent. |
| `refused` | The node rejected the submission. |
| `lost` | Accepted or seen in a best block, absent from the finalized chain, and its signer's finalized account nonce has passed its nonce. |
| `unverified` | Seen in a best block, but neither finalization nor loss is proven. |
| `unknown` | Not enough evidence for another status. |

A reorg can drop a best block; a finalized block is permanent. Without evidence of loss, a transaction stays `unknown` or `unverified`. An accepted transaction whose nonce is not yet used may still finalize, even when absent from READY. The summary counts these transactions. Unknown or unverified transactions make the no-loss check INCONCLUSIVE. A lost transaction, or one that failed after inclusion, makes the verdict FAIL.

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
| Pool intake: the node, or the network between, answered or read submissions too slowly, with bytes unsent on an open connection | graceful |
| Slow blocks: mean block gap, by the chain's own timestamps, above `max-block-gap-factor` times `block-interval-seconds` | hard |
| Stall: no new best block for the limit, not counting time the node didn't answer or PolkaMeter didn't observe | hard |
| Finality stall: no new finalized block for the limit, the same way | hard |
| Node down: the node restarted, or it answered nothing for 15 s and its process on this machine shows that it failed | hard |
| Rate cap: every rate step ran | end, no failure |
| Budget used up: the prepared flood ran out before the last rate step finished | end, no failure |
| Generator limit: PolkaMeter sent too little of the target rate, paused more than 5 s, or had no connection open. Or its connection kept dropping, or the node or network answered nothing for 15 s | end, no failure |
| Smoke error: a monitor problem, in smoke mode only | end, no failure |

A graceful stop ends the load when the node refuses or delays transactions but still makes blocks. A hard stop ends the load when the node goes down, or blocks or finality slow down or stop for everyone. Stall, finality stall and node down also hold in recovery and while the loss check runs, and fail the run there.

A dropped connection that reconnects is not a stop. See [blocks: RPC connection lost](troubleshooting.md#blocks-rpc-connection-lost--reconnected-after-seconds-s-nothing-observed-meanwhile).

## Whose fault is it

PolkaMeter watches the chain from one place, through its own connections and clock. It asserts only what that evidence shows:

- A. Never the chain for a fault outside it: a pause of PolkaMeter, the network or a proxy in between, or a reconnect. Block pace comes from the chain itself. Slow blocks use the chain's own timestamps, and a stall is no new block while the node keeps answering. A delay on the way can't fake either.
- B. Never excuse the chain for a fault in it. A stall, a finality stall or node down fails the run in any phase, also when PolkaMeter paused or reconnected too. Node down needs evidence that it was the node: it came back restarted, or its process on this machine ended, is stopped, or serves the RPC port itself.
- C. No evidence, no number. A measure PolkaMeter couldn't observe has no result, never a stand-in such as 0 ms. The verdict is then at most INCONCLUSIVE.
- D. Each sent flood transaction gets one loss status: sent = finalized + in pool + refused + lost + unverified + unknown.

What one place can't show:

- If the node, or the network between, answers nothing for 15 s, PolkaMeter can't tell which failed. Without the evidence of rule B, the load ends on `generator limit: node or network unreachable`, and the verdict is INCONCLUSIVE. A connection that stays open with no answer counts the same as one that closed.
- Latency and reply times are measured from PolkaMeter's side. They include the network between PolkaMeter and the node, and PolkaMeter can't see a delay on an open connection. Run PolkaMeter close to the node.
- PolkaMeter detects its own pauses longer than about 1 s, and leaves out every time measured across one. A shorter pause goes unseen and can add up to that much to a time.

In a run, this means:

1. A disconnection shorter than 15 s is never node down, in the load or the loss check. A connection that keeps dropping, while the node doesn't restart, ends the load on `generator limit: connection unstable`.
2. The node's identity after a reconnect tells a restart: the same peer ID with a new sync starting block. A restarted proxy changes neither. A new peer ID is another node behind a load balancer, and only a warning.
3. A transaction, reply or probe timed across a pause of PolkaMeter, or read late after a reconnect, counts but has no time. Latency, reply time, the baseline and the exports leave it out, and the `Warnings:` line counts it. A pause of more than 5 s ends the load on `generator limit`.
4. A baseline with no timed probe has no result. A recovery PolkaMeter didn't observe until the chain was back is `not measured`, with no drain rate.
5. Time with no connection open is never pool intake or latency. A transaction sent again after its connection dropped has no latency.

The default `fail-on` policy, `verdict-or-inconclusive`, fails on INCONCLUSIVE too. So a node or network that was unreachable still fails a CI job, without a FAIL verdict.

Tests guard each rule. `AGENTS.md` in the repository names them.
