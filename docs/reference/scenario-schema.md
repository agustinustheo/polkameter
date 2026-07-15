# Scenario schema

All field names are camelCase unless an enum representation is shown. `version` is currently `1`.

| Path | Type | Required | Meaning / constraints |
|---|---|---:|---|
| `version` | number | yes | `1`; legacy `0` migrates on load. |
| `testPlan.name` | string | yes | Non-empty name. |
| `testPlan.description` | string | yes | Human description. |
| `testPlan.seed` | unsigned number | yes | Seed for deterministic arrival offsets. |
| `testPlan.limits.wholeRunTimeoutMs` | number | yes | At least 1,000. |
| `testPlan.limits.shutdownDrainTimeoutMs` | number | yes | At least 1,000. |
| `testPlan.limits.maxConcurrentSamples` | number | yes | At least 1; shared transaction ceiling. |
| `chain.endpoint` | string | yes | `ws://` or `wss://` RPC URL. |
| `chain.prometheusEndpoint` | string | no | `http://` or `https://` metrics URL. |
| `chain.transactionProfile` | string | no | Defaults to `polkadot`; this build executes the standard Polkadot profile. |
| `signerSource.profile` | string | yes | Credential-vault alias; non-empty. |
| `signerSource.baseSuri` | string | no | Persist only `[redacted]`; resolved at runtime. |
| `signerSource.derivationPath` | string | no | Defaults to `//polkameter`. |
| `signerSource.funding` | object | no | Local development funding only. |
| `threadGroups` | array | yes | At least one group. |
| `collectors` | array | yes | `jtl`, `events_jsonl`, `telemetry_jsonl`, `summary`, and/or `svg_plots`. |

## Funding object

| Path | Type | Constraints |
|---|---|---|
| `amount` | decimal string | Positive balance. |
| `finalityTimeoutMs` | number | At least 1,000; defaults to 60,000. |
| `batchSize` | number | 1–100; defaults to 50. |

Funding additionally requires a loopback `ws://` endpoint. It is not a general account-management feature.

## Thread group

| Path | Type | Constraints |
|---|---|---|
| `name` | string | Non-empty. |
| `users` | unsigned number | At least 1. |
| `concurrency` | unsigned number | From 1 through `users`. |
| `iterations` | unsigned number | At least 1; defaults to 1. |
| `arrival` | object | One of the forms below. |
| `samplers` | array | At least one sampler. |

Arrival forms are `{"kind":"burst","windowMs":N}`, `{"kind":"ramp","durationMs":N}`, and `{"kind":"poisson","ratePerSecond":N}`. Burst/ramp values must be positive; the Poisson rate must be positive.

## Sampler

| Path | Type | Constraints |
|---|---|---|
| `phase` | string | `setup`, `transaction`, or `teardown`. |
| `label` | string | Non-empty display/report label. |
| `pallet` / `call` | string | Non-empty; preflight confirms live metadata/encoding. |
| `arguments` | object or array | Dynamic SCALE inputs; see [Scenarios](../using/scenarios.md). |
| `completion` | string | `submitted`, `in_block`, or `finalized`. |
| `mortalityPeriod` | number | Power of two, at least 4. |
| `finalityTimeoutMs` | number | At least 1,000. |
| `assertions` | array | `{"kind":"success"}` and/or `{"kind":"max_elapsed","milliseconds":N}`. |
