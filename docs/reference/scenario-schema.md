# XML plan schema

The authoritative machine-readable contract is [`schemas/polkameter-plan-v1.xsd`](https://github.com/agustinustheo/polkameter/blob/main/schemas/polkameter-plan-v1.xsd). This page is a field-oriented companion for review and authoring. XML plans must use namespace `https://polkameter.dev/schema/plan/v1` and `version="1"`.

## Root and plan

| Element / attribute | Required | Meaning / constraints |
|---|---:|---|
| `<polkameter-plan version>` | yes | Positive version; this implementation supports `1`. |
| `<test-plan name seed>` | yes | Non-empty name and deterministic non-negative seed. |
| `<description>` | no | Human plan description. |
| `<limits whole-run-timeout-ms shutdown-drain-timeout-ms max-concurrent-samples>` | yes | Positive limits; runtime requires both timeouts to be at least 1,000 ms. |
| `<chain endpoint transaction-profile>` | yes | WebSocket RPC and `polkadot` or `custom:<name>` profile. This build executes the standard Polkadot profile. |
| `<prometheus endpoint>` | no | HTTP(S) metrics endpoint. |
| `<signer profile derivation-path>` | yes | Profile alias and derivation root only—never a SURI. |
| `<funding amount finality-timeout-ms batch-size>` | no | Local-development-only funding configuration. |

## User groups and arrival

| Element / attribute | Required | Meaning / constraints |
|---|---:|---|
| `<user-group name users concurrency iterations>` | yes | Non-empty name; at least one user/iteration; concurrency from 1 through users. |
| `<arrival kind="burst" window-ms>` | one | Positive window. |
| `<arrival kind="ramp" duration-ms>` | one | Positive duration. |
| `<arrival kind="poisson" rate-per-second>` | one | Positive decimal rate. |
| `<setup>` | no | Ordered calls executed once before the scheduled workload. |
| `<workflow>` | no | Ordered calls executed for each virtual user and iteration. |
| `<teardown>` | no | Ordered calls executed once after workload drain. |

Every group must contain at least one call across its phase containers.

## Calls and assertions

| Element / attribute | Required | Meaning / constraints |
|---|---:|---|
| `<call label pallet method>` | yes | Non-empty report label and selected runtime pallet/method. |
| `completion` | yes | `submitted`, `in-block`, or `finalized`. |
| `mortality-period` | yes | Power of two, at least 4. |
| `finality-timeout-ms` | yes | At least 1,000. |
| `<arguments>` | yes | JSON object or array of dynamic SCALE inputs. |
| `<assertion kind="success">` | no | Requires a successful transaction outcome. |
| `<assertion kind="max-elapsed" milliseconds>` | no | Fails a sample exceeding its latency limit. |

Use `method`, not `call`, in XML. See [XML test plans](../using/scenarios.md) for JSON-in-XML conventions and an end-to-end example.

## Collectors

`<collectors>` must contain at least one `<collector kind="…"/>`. Supported XML names are `jtl`, `events-jsonl`, `telemetry-jsonl`, `summary`, and `svg-plots`. They map to the portable report evidence described in [Artifacts and reports](../using/artifacts.md).

## Legacy JSON

The CLI can still load legacy `.polkameter.json` documents. Their fields use camelCase and underscore enum values, but XML is the preferred saved and reviewed format. Any converted or newly created plan should use XML rather than depending on that compatibility path.
