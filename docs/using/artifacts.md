# Artifacts and reports

Every completed or failed local run creates a directory named `run-<timestamp>` inside the output root. The desktop, CLI, and agent all produce the same portable, redacted contract. This makes results transferable between a stress host, CI artifact storage, and a colleague's workstation.

## Artifact contract

| File | Contents |
|---|---|
| `scenario.polkameter.json` | Redacted source scenario. |
| `resolved-plan.json` | Redacted scenario plus run ID, signer derivation root, required signer count, scheduled samples, and scheduler description. |
| `config.json` | The same resolved-plan contract for compatibility. |
| `command.txt` | Provenance: desktop, CLI, or remote agent origin. |
| `samples.jtl` | CSV/JMeter-style per-sample records, including timestamps, elapsed time, code, message, thread, success, bytes, and concurrency fields. |
| `events.jsonl` | Line-delimited event evidence: account, phase, scheduling/submission/completion times, hashes, outcome, and message. |
| `telemetry.jsonl` | Line-delimited runner and optional node telemetry. |
| `summary.md` | Human-readable aggregate report. |
| `plots/*.svg` | Throughput, latency percentiles, failure breakdown, CPU/memory, blocks/pending, and node-resource plots. |

`polkameter report <artifact-dir>` verifies this contract before printing the report. Validation checks required files and non-empty plots, parses the JSON documents, and confirms all signer sources are redacted. It is a useful CI postcondition before uploading or publishing an artifact.

## Summary metrics

The generated Markdown summary reports total, successful, failed, and timed-out samples; maximum sample elapsed time; p50/p95/p99 latency; maximum runner CPU/RSS; maximum pending extrinsics; and final best/finalized block values. When Prometheus data exists, it also reports node maximum RSS, CPU, and ready transactions.

The percentiles are calculated from sample elapsed values. Use the raw JTL and JSONL evidence when you need to reproduce a calculation or correlate a failure with chain events.

## Telemetry semantics

The runner records its own process CPU/memory and chain status. When `chain.prometheusEndpoint` is configured, it also attempts to collect node RSS/CPU and `substrate_ready_transactions_number`. An unavailable metric is recorded as absent, not as zero; check `rpc_error` or `prometheus_error` before interpreting missing values.

## Retention guidance

Upload the entire directory for significant test runs, not just the summary. The redacted plan and resolved plan make workload reproduction possible, while JTL/event/telemetry data makes a claim auditable. Artifact folders contain operational details such as account addresses and endpoint metadata; apply your organization's access and retention policy.
