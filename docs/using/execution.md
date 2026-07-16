# Scheduling and execution

Polkameter separates *when work becomes eligible* from *how much can run at once*. This is important when interpreting throughput: an aggressive arrival model may still be throttled by the group concurrency or plan-wide ceiling.

## Arrival models

| Model | Scenario form | Behavior |
|---|---|---|
| Burst | `{ "kind": "burst", "windowMs": 1000 }` | Releases every virtual user at a seeded random offset within the window. |
| Ramp | `{ "kind": "ramp", "durationMs": 5000 }` | Spreads users evenly from 0 to the duration; one user starts at 0. |
| Poisson | `{ "kind": "poisson", "ratePerSecond": 20 }` | Uses seeded exponential inter-arrival gaps at the given rate. |

Offsets are sorted and deterministic for a given `testPlan.seed`, group, and workload. The desktop preview therefore represents the same schedule that a run will use. Burst and ramp durations must be non-zero; Poisson rates must be positive.

## Phases, users, and iterations

For each group, setup calls execute once, XML `<workflow>` calls (the runner's internal transaction phase) execute for every scheduled virtual user and iteration, and teardown calls execute once. The resolved-plan sample count is:

```text
setup calls + (users × iterations × workflow calls) + teardown calls
```

Each group gets a disjoint signer-index range. The runner processes all groups concurrently in the transaction phase, but setup and teardown group work is sequential.

## Concurrency limits

There are two active controls:

- `threadGroups[].concurrency` limits concurrent transaction-user tasks inside that group.
- `testPlan.limits.maxConcurrentSamples` is a shared semaphore across all transaction groups.

The lowest applicable availability controls a submission. A plan can therefore have 1,000 virtual users but a much smaller safe submission ceiling.

## Completion boundaries and assertions

Every sampler chooses a completion boundary:

- `submitted`: the transaction was submitted/broadcast.
- `in_block`: it appeared in a block.
- `finalized`: its containing block finalized.

Each sample also has `finalityTimeoutMs`. `success` asserts a successful transaction outcome. `max_elapsed` additionally fails a sample that exceeds the configured milliseconds. Failed or timed-out samples do not hide the generated evidence: they are recorded in the JTL, events stream, report, and final run status.

## Run limits and stop behavior

`wholeRunTimeoutMs` bounds the whole execution. `shutdownDrainTimeoutMs` bounds what can remain after a requested stop. The runner records a status lifecycle including `draft`, `arming`, `running`, `stopping`, `completed`, `completed_with_failures`, `stopped`, and `failed`.

Before becoming `running`, it connects, optionally funds permitted development accounts, and checks derived signer readiness. A failed connection or readiness check produces a failed artifact rather than silently proceeding.
