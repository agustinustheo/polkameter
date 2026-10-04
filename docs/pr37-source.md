# PR 37 source attribution

The crates under `crates/{chain,files,load,monitors,checks}` and `plugins/{proofs,scenarios}` derive from paritytech/polkadot-pop-e2e PR 37, commit `8d2a060071c705654be8d3ae9dc668b4633469c0`, under Apache-2.0. The original workspace is https://github.com/paritytech/polkadot-pop-e2e/tree/8d2a060071c705654be8d3ae9dc668b4633469c0/stress.

The migration retains the original fixtures and most implementation code, with the deliberate evidence changes below. Polkameter replaces the standalone CLI with XML orchestration and a versioned plugin protocol. Domain transaction encoding and setup are isolated in the People package.

## Deliberate deviations from the pinned source

- A closed RPC connection without a reply leaves the submission **unknown**, not refused: the node may already have accepted it.
- Recovery requires at least one observed block gap; missing block evidence cannot establish healthy production.
- Finalized dispatch events distinguish successful inclusion from `ExtrinsicFailed`; an RPC reply alone cannot establish success.
- Reconciliation snapshots the ready pool **before** waiting/walking, then requires finality through at least the best head observed after that snapshot. The earlier migrated late-listing order was a regression and is removed. In-pool records describe that snapshot, not the later end of report generation.
- OpenMetrics names use `polkameter_*` instead of `stress_*`. Existing dashboards and recording rules must update their selectors; no alias metric family is emitted.
- Lost-transaction descriptions are not exposed by the current external state-check protocol, so `lost.jsonl.scenario` is null. Prepared artifacts retain the hash-to-claim metadata.
- The XML People operation currently uses the default prover pool rather than exposing the upstream `threads` option. Step durations retain total preparation time, but separate ring-wait/proving timings are not in the plugin output.
- Unused mortal-era encoding and its setter were removed; all production callers use immortal transactions. Existing claim/signing byte vectors still cover the wire format.
- The PR's XOR target mapping is preserved for comparison. It has at most 256 distinct targets, so allowance sampling is weak corroboration, not proof of individual claim execution. Unique hashed targets require a separate workload version and a new performance baseline.
- XML declares the expected block interval. During preflight, before recognition or proving in setup, the host measures it using the upstream timestamp-span method, records both values in `calibration.json`, and rejects deviations over 25%. The validated configured value drives the probe budget and recovery cadence so the prepared workload remains reproducible.
- Plugin state checks cache one RPC client and one immutable hash-pinned state artifact per process instead of reconnecting/reparsing for every lost transaction. A typed transport failure triggers one reconnect and read-only retry at the same finalized hash.

- `setupSeconds` in Polkameter starts before plugin startup/live checks; upstream starts its timer after preflight. Compare phase records rather than treating these two totals as the same timing boundary.
- `loss.unknown` includes accepted submissions when reconciliation evidence is incomplete. For an exclusive accounting identity use `transactions.jsonl` statuses (or finalized `loss.onChain.included` where available), not the best-chain `loss.included` observation count.
