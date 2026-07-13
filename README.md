# Polkameter

Polkameter is a Tauri desktop workbench for designing generic Polkadot SDK transaction stress scenarios. Its interaction model is inspired by JMeter: compose a test plan, configure virtual users and an arrival model then preflight and run the plan against a local chain.

## Current capabilities

The Rust execution core owns a versioned native scenario model, deterministic seeded Burst, Ramp and Poisson schedules, per-group concurrency plus a plan-wide concurrency ceiling, graceful cancellation and JTL-compatible results. The local Subxt runtime adapter owns chain connection, signer derivation, readiness checks and transaction watching; Tauri only exposes commands and forwards run events. The current adapter uses the standard `PolkadotConfig` transaction profile.

Before arming a run, Polkameter creates a run ID, connects to the configured WebSocket RPC, reads live runtime metadata, lists pallet calls, shows selected call fields and asks Subxt to SCALE encode every dynamic transaction. The runner derives development signers in memory from that same run ID, submits transactions and records submitted, in-block or finalized outcomes.

Each run writes a portable, redacted artifact directory:

- `scenario.polkameter.json` and `resolved-plan.json`
- `config.json` and `command.txt`
- `samples.jtl`, `events.jsonl` and `telemetry.jsonl`
- `summary.md`
- `plots/throughput.svg`, `latency-percentiles.svg` and `failure-breakdown.svg`
- `plots/cpu-memory.svg`, `blocks-pending.svg` and `node-resources.svg`

Saved plans and artifacts contain only a `signerProfile` alias, never a SURI. The desktop app writes a SURI to the native operating-system credential vault and resolves that alias inside Rust immediately before preflight or arm/run; the renderer does not reload it when a plan is reopened. Each thread group receives a disjoint signer range; non-base virtual users derive under a run-specific root, so neither groups nor runs collide. Transaction groups run concurrently, while a shared plan-wide limit prevents their combined pressure from exceeding the configured ceiling. Preflight shows the exact root and accounts that the following arm/run operation will use. Before arming, every virtual signer required by the plan must have a `System.Account` record; unfunded users fail before any submissions are scheduled.

The optional `Fund derived users` helper is deliberately limited to loopback `ws://` endpoints and development SURIs beginning with `//`. It prepares a fresh local dev chain through bounded `Utility.batch_all` calls (default: 50 recipients per finalized batch); it cannot be used as a remote-chain funding mechanism.

An optional `Node Prometheus` endpoint records node metrics separately from the Polkameter process and RPC health. When exposed, standard `process_resident_memory_bytes` and `process_cpu_seconds_total` produce node host RSS/CPU data; Substrate's `substrate_ready_transactions_number` is also collected when present. Missing metrics are represented as absent data, not zero, so a node that exports only transaction-pool telemetry remains visible without inventing host-resource values.

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

The desktop header and native application icon use the monochrome Polkadot mark, paired with a
black, white and stone workbench palette.

The execution path is:

1. Chain connection and runtime metadata preflight.
2. Deterministic signer-pool preparation.
3. Dynamic pallet/call encoding and bounded submission.
4. Submitted, in-block or finalised sample collection. Transaction samplers support a bounded loop count per virtual user; setup and teardown remain single-run phases. A Stop request halts scheduling and grants active watches only the configured shutdown-drain deadline.
5. JTL-compatible samples, event logs, telemetry and real SVG plots.
6. Structural JMX import and export for test-plan, thread-group and collector interchange. The
   desktop's `Inspect JMX` control reports imported JMeter structure and unsupported sampler
   types without executing them. Exported `.jmx` files remain companions to the authoritative
   `.polkameter.json` file because generic JMeter samplers do not carry a Polkadot pallet, call
   and SCALE-encoding contract.

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

The ignored integration test proves the full generic path from a fresh local node: save and reopen a redacted native scenario, preflight its dynamic call, fund five run-derived development accounts across two transaction groups, arm asynchronously, stream status/sample events and validate the resulting artifacts. The fixture asks for five aggregate users while the plan-wide concurrency ceiling is two. It requires a fresh local dev chain at `ws://127.0.0.1:9944`:

```sh
polkadot --dev --tmp --rpc-port 9944 --prometheus-port 9615 --rpc-methods Unsafe --rpc-cors all
```

In another terminal:

```sh
POLKAMETER_E2E_RPC=ws://127.0.0.1:9944 \
	POLKAMETER_E2E_PROMETHEUS=http://127.0.0.1:9615/metrics \
  POLKAMETER_E2E_OUTPUT_ROOT="$(pwd)/src-tauri/target/polkameter-e2e" \
  cargo +1.93.0 test --manifest-path src-tauri/Cargo.toml \
  fresh_dev_chain_run_writes_validated_artifacts -- --ignored --nocapture
```

The retained run directory contains the full artifact contract and can be opened from the desktop's run-report control after a desktop run.

The same fixture is the generic high-scale acceptance command. It continues to start from an
empty chain, funds run-specific accounts through finalized batches, and runs both transaction
groups concurrently. For the 1,000-user burst proof, use one request per user and let the
plan-wide limit admit all 1,000 submissions:

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

The 1,000-user configuration has 1,000 scheduled transaction samples across two concurrent
groups. It is deliberately a burst, not a ramp: both groups use the scenario's seeded one
millisecond burst arrival window.

## Remote agent

The same binary can run an authenticated remote agent:

```bash
POLKAMETER_AGENT_TOKEN="replace-with-a-long-random-token" \
POLKAMETER_AGENT_OUTPUT_ROOT="target/polkameter-agent-runs" \
polkameter agent
```

The agent binds to `127.0.0.1:9901` by default. Keep that default and use an SSH tunnel from the desktop, or place a TLS terminator in front of the agent. The desktop accepts `http://` only for loopback endpoints and otherwise requires `https://`.

The remote request contains a redacted scenario and a run ID only. The remote host resolves the configured signer profile from its own operating-system credential store, performs metadata preflight locally, and retains its own artifacts. For headless deployment, inject `POLKAMETER_AGENT_SURI` into the agent process through its host secret manager; it is used only on the agent and never accepted over HTTP. The remote runner URL and bearer token are session-only desktop fields; they are not stored in a `.polkameter.json` scenario or browser storage.

## Current Boundary

This is deliberately chain-generic. DIM2-specific game setup, funding, phase transitions and result assertions belong in adapters or scenario extensions rather than the core test-plan model. The runner currently supports the standard Polkadot transaction profile, native credential-vault signer profiles, optional node Prometheus telemetry and structural JMX import/export. JMX import reports thread groups and collectors for inspection; it never executes a non-Substrate JMeter sampler. External workers and domain adapters remain future work.
