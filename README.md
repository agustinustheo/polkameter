# Polkameter

Polkameter is a Tauri desktop workbench for designing generic Polkadot SDK transaction stress scenarios. Its interaction model is inspired by JMeter: compose a test plan, configure virtual users and an arrival model then preflight and run the plan against a local chain.

## Current capabilities

The Rust execution core owns a versioned native scenario model, deterministic seeded Burst, Ramp and Poisson schedules, bounded concurrency, graceful cancellation and JTL-compatible results. The local Subxt runtime adapter owns chain connection, signer derivation, readiness checks and transaction watching; Tauri only exposes commands and forwards run events. The current adapter uses the standard `PolkadotConfig` transaction profile.

Before arming a run, Polkameter creates a run ID, connects to the configured WebSocket RPC, reads live runtime metadata, lists pallet calls, shows selected call fields and asks Subxt to SCALE encode every dynamic transaction. The runner derives development signers in memory from that same run ID, submits transactions and records submitted, in-block or finalized outcomes.

Each run writes a portable, redacted artifact directory:

- `scenario.polkameter.json` and `resolved-plan.json`
- `config.json` and `command.txt`
- `samples.jtl`, `events.jsonl` and `telemetry.jsonl`
- `summary.md`
- `plots/throughput.svg`, `latency-percentiles.svg` and `failure-breakdown.svg`
- `plots/cpu-memory.svg` and `blocks-pending.svg`

Development SURIs are deliberately redacted from saved browser drafts and every persisted artifact. The base development signer is retained only in memory. Each thread group receives a disjoint signer range; non-base virtual users derive under a run-specific root, so neither groups nor runs collide. Preflight shows the exact root and accounts that the following arm/run operation will use. Before arming, every virtual signer required by the plan must have a `System.Account` record; unfunded users fail before any submissions are scheduled.

## Scenario Shape

A native scenario has test-plan metadata and run limits, one or more thread groups and ordered setup, transaction and teardown samplers. Each sampler has its own pallet, call, JSON arguments, completion boundary, mortality period, finality timeout and optional maximum-elapsed assertion. Collector selection is also saved with the plan.

Dynamic values can use these explicit markers when a plain JSON value is ambiguous:

```json
{
  "dest": {
    "$variant": "Id",
    "value": { "$bytes": "0xd43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27d" }
  },
  "value": "1000000000000"
}
```

The decimal string is converted to an unsigned SCALE integer. `$variant` and `$bytes` represent enum and byte values without relying on pallet-specific code generation.

The desktop build uses Tauri's stock template icon temporarily. A Polkameter icon should replace it before a distributable bundle is produced.

The execution path is:

1. Chain connection and runtime metadata preflight.
2. Deterministic signer-pool preparation.
3. Dynamic pallet/call encoding and bounded submission.
4. Submitted, in-block or finalised sample collection. A Stop request halts scheduling and grants active watches only the configured shutdown-drain deadline.
5. JTL-compatible samples, event logs, telemetry and real SVG plots.

## Run locally

```sh
corepack pnpm install
corepack pnpm tauri dev
```

Run the frontend and Rust checks without opening the desktop app:

```sh
corepack pnpm build
cargo test --manifest-path src-tauri/Cargo.toml
```

## Fresh-chain Acceptance

The ignored integration test proves the full generic path: save a redacted native scenario, reopen it, restore the in-memory development SURI, metadata-preflight its dynamic call, arm it and validate its artifacts after a finalized transfer. It requires a fresh local dev chain at `ws://127.0.0.1:9944`:

```sh
polkadot --dev --tmp --rpc-port 9944 --rpc-methods Unsafe --rpc-cors all
```

In another terminal:

```sh
POLKAMETER_E2E_RPC=ws://127.0.0.1:9944 \
  POLKAMETER_E2E_OUTPUT_ROOT="$(pwd)/src-tauri/target/polkameter-e2e" \
  cargo +1.93.0 test --manifest-path src-tauri/Cargo.toml \
  fresh_dev_chain_run_writes_validated_artifacts -- --ignored --nocapture
```

The retained run directory contains the full artifact contract and can be opened from the desktop's run-report control after a desktop run.

## Current Boundary

This is deliberately chain-generic. DIM2-specific game setup, funding, phase transitions and result assertions belong in adapters or scenario extensions rather than the core test-plan model. The runner currently supports only local standard-transaction-profile chains. Remote endpoints, vault-backed signers, external workers, Prometheus export, JMX compatibility and domain adapters are intentionally deferred.
