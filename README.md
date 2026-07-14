<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/logo-dark.png">
    <img src="docs/logo-light.png" alt="Polkameter" width="380">
  </picture>
</p>

<p align="center">
  <a href="https://github.com/agustinustheo/polkameter/actions/workflows/ci.yml?query=branch%3Amain"><img src="https://github.com/agustinustheo/polkameter/actions/workflows/ci.yml/badge.svg?branch=main" alt="CI"></a>
  <a href="LICENSE"><img src="https://img.shields.io/github/license/agustinustheo/polkameter?style=flat-square" alt="Apache-2.0 license"></a>
  <a href="https://github.com/agustinustheo/polkameter/graphs/contributors"><img src="https://img.shields.io/github/contributors/agustinustheo/polkameter?style=flat-square" alt="Contributors"></a>
  <a href="https://github.com/agustinustheo/polkameter/stargazers"><img src="https://img.shields.io/github/stars/agustinustheo/polkameter?style=flat-square" alt="Stars"></a>
</p>

<img width="1428" height="917" alt="Polkameter Dashboard" src="https://github.com/user-attachments/assets/fad26abd-92fc-49a8-9fa2-ee3773b30703" />

Polkameter is a Tauri desktop workbench for stress-testing Polkadot SDK chains, modeled on JMeter: compose a test plan with thread groups and samplers, pick an arrival model, preflight against a live chain, then arm and run. The Rust core owns scheduling, signing, submission and artifacts; the TypeScript frontend is only an editor and monitor.

## Run

```sh
corepack pnpm install
corepack pnpm tauri dev
```

Checks without opening the desktop app:

```sh
corepack pnpm test                                   # frontend unit tests
corepack pnpm build                                  # tsc + vite build
cargo test --manifest-path src-tauri/Cargo.toml      # Rust core tests
```

## Command line

The release includes a headless `polkameter` command alongside the desktop app. It uses the same scenario format, Subxt runner, telemetry, artifacts and reports as the UI, but it never opens a window.

```sh
# Validate a portable, redacted scenario without touching a chain.
polkameter validate scenario.polkameter.json

# Resolve a signer profile from the OS credential vault and preflight live metadata.
polkameter preflight scenario.polkameter.json --signer-profile local-dev

# Run locally. Progress is human-readable on stderr; artifacts are written below target/runs.
polkameter run scenario.polkameter.json \
  --signer-profile local-dev \
  --output target/runs

# Use a named environment variable in CI; neither scenarios nor command arguments contain a SURI.
POLKAMETER_SURI='//Alice' \
  polkameter run scenario.polkameter.json \
  --signer-env POLKAMETER_SURI \
  --output target/runs \
  --format json

# Validate an existing artifact bundle and print its report.
polkameter report target/runs/run-123 --format json
```

`run` preflights before arming. Successful local runs write the same portable artifact contract as the UI. Its exit status is `0` for success, `2` for invalid input, `3` for signer or preflight failures, `4` for completed runs with failed/timed-out samples, and `130` after `Ctrl-C` requests a graceful drain.

For a remote worker, start the agent on the stress machine. It binds only to loopback, so expose it with SSH forwarding or TLS termination. The agent resolves its own signer profile or `POLKAMETER_AGENT_SURI`; callers only transmit a redacted scenario.

```sh
POLKAMETER_AGENT_TOKEN='long-random-token' \
  polkameter agent serve --output-root target/polkameter-agent-runs

POLKAMETER_REMOTE_TOKEN='long-random-token' \
  polkameter run scenario.polkameter.json \
  --remote http://127.0.0.1:9901 \
  --remote-token-env POLKAMETER_REMOTE_TOKEN \
  --format json
```

Remote artifacts remain on the agent under its configured output root; the CLI confirms the remote report only after the agent validates the artifact contract.

A run needs a chain to target. For local work start a fresh dev node:

```sh
polkadot --dev --tmp --rpc-port 9944 --prometheus-port 9615 --rpc-methods Unsafe --rpc-cors all
```

## How a run works

1. Preflight connects to the WebSocket RPC, reads live runtime metadata and SCALE encodes every dynamic call. A failed encoding blocks arming.
2. Signers derive deterministically from the run ID; each thread group gets a disjoint range. Every virtual signer must have a `System.Account` record before submission starts.
3. Groups run concurrently under per-group concurrency plus a plan-wide ceiling. Arrival models (seeded Burst, Ramp, Poisson) are deterministic. Stop halts scheduling and drains active watches within the configured deadline.
4. Samples record submitted, in-block or finalized outcomes.

Each run writes a portable, redacted artifact directory: `scenario.polkameter.json`, `resolved-plan.json`, `config.json`, `command.txt`, `samples.jtl`, `events.jsonl`, `telemetry.jsonl`, `summary.md` and SVG plots (throughput, latency percentiles, failure breakdown, node resources).

## Scenarios

A scenario is test-plan metadata, run limits, thread groups and ordered setup/transaction/teardown samplers, each with its own pallet, call, JSON arguments, completion boundary, mortality period and timeouts. Where plain JSON is ambiguous, use explicit markers:

```json
{
  "dest": {
    "$variant": "Id",
    "value": { "$bytes": "0xd43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27d" }
  },
  "value": "1000000000000"
}
```

`$variant` and `$bytes` represent enum and byte values; decimal strings become unsigned SCALE integers. No pallet-specific code generation is required.

## Signers and funding

Saved plans and artifacts contain only a `signerProfile` alias, never a SURI. The desktop stores the SURI in the operating-system credential vault and Rust resolves it just before preflight or run. The optional `Fund derived users` helper only accepts loopback `ws://` endpoints and development SURIs starting with `//`; it funds run-derived accounts through bounded `Utility.batch_all` calls and cannot fund remote chains.

## Telemetry and JMX

An optional `Node Prometheus` endpoint collects node RSS/CPU and `substrate_ready_transactions_number` alongside run telemetry; missing metrics are recorded as absent, not zero. Scenarios export to structural `.jmx` companions and `Inspect JMX` reports imported JMeter structure without executing non-Substrate samplers. The `.polkameter.json` file stays authoritative because JMX carries no pallet or SCALE contract.

## Acceptance test

An ignored integration test proves the full path against a fresh dev node (save/reopen, preflight, fund five accounts across two groups, run, validate artifacts):

```sh
POLKAMETER_E2E_RPC=ws://127.0.0.1:9944 \
POLKAMETER_E2E_PROMETHEUS=http://127.0.0.1:9615/metrics \
POLKAMETER_E2E_OUTPUT_ROOT="$(pwd)/src-tauri/target/polkameter-e2e" \
cargo +1.93.0 test --manifest-path src-tauri/Cargo.toml \
  fresh_dev_chain_run_writes_validated_artifacts -- --ignored --nocapture
```

For the 1,000-user burst proof (one request per user, plan-wide limit admitting all submissions):

```sh
POLKAMETER_E2E_RPC=ws://127.0.0.1:9944 \
POLKAMETER_E2E_PROMETHEUS=http://127.0.0.1:9615/metrics \
POLKAMETER_E2E_OUTPUT_ROOT="$(pwd)/src-tauri/target/polkameter-e2e-1000" \
POLKAMETER_E2E_USERS=1000 \
POLKAMETER_E2E_ITERATIONS=1 \
POLKAMETER_E2E_CONCURRENCY=1000 \
POLKAMETER_E2E_MAX_CONCURRENT_SAMPLES=1000 \
POLKAMETER_E2E_FUNDING_BATCH_SIZE=100 \
POLKAMETER_E2E_TEST_TIMEOUT_SECS=900 \
cargo +1.93.0 test --manifest-path src-tauri/Cargo.toml \
  fresh_dev_chain_run_writes_validated_artifacts -- --ignored --nocapture
```

Both need a fresh local dev chain at `ws://127.0.0.1:9944` (command above).

## Boundary

Deliberately chain-generic: the standard `PolkadotConfig` transaction profile, credential-vault signer profiles, optional Prometheus telemetry and structural JMX interchange. Domain-specific setup, funding and assertions belong in adapters or scenario extensions, not the core plan model.
