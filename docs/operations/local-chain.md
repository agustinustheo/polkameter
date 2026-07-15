# Testing a local chain

Use a fresh local chain whenever you change scenario semantics, signing behavior, or runner behavior. It gives repeatable state and avoids accidental impact on a shared endpoint.

## Manual dev node

```sh
polkadot --dev --tmp --rpc-port 9944 --prometheus-port 9615 --rpc-methods Unsafe --rpc-cors all
```

Then run the [first local run](../first-run.md), optionally enabling the scenario's local-only funding helper.

## Ignored integration tests

The Rust integration test exercises save/reopen, preflight, development funding, execution, and artifact validation against a fresh node. It is ignored by default because it requires the chain:

```sh
POLKAMETER_E2E_RPC=ws://127.0.0.1:9944 \
POLKAMETER_E2E_PROMETHEUS=http://127.0.0.1:9615/metrics \
POLKAMETER_E2E_OUTPUT_ROOT="$(pwd)/src-tauri/target/polkameter-e2e" \
cargo +1.93.0 test --manifest-path src-tauri/Cargo.toml \
  fresh_dev_chain_run_writes_validated_artifacts -- --ignored --nocapture
```

The larger burst proof accepts environment overrides such as `POLKAMETER_E2E_USERS=1000`, `POLKAMETER_E2E_ITERATIONS=1`, `POLKAMETER_E2E_CONCURRENCY=1000`, `POLKAMETER_E2E_MAX_CONCURRENT_SAMPLES=1000`, and a larger test timeout. Use those numbers only when your machine and development chain can safely absorb the load.

## Zombienet smoke test

`tests/zombienet/cli-smoke.sh` owns a fresh native relay. It validates, preflights, runs, and reports locally, then repeats the preflight/run/report path through a loopback remote agent. It refuses to run if its dedicated RPC (`19144`), Prometheus (`19161`), or agent (`19901`) port is occupied.

Install the pinned binaries, then run:

```sh
TOOLS_DIR="$(tests/zombienet/install-binaries.sh)"
POLKAMETER_ZOMBIENET_BIN="$TOOLS_DIR/zombie-cli" \
POLKAMETER_POLKADOT_BIN="$TOOLS_DIR/polkadot" \
POLKAMETER_ZOMBIENET_KEEP_ARTIFACTS=1 \
tests/zombienet/cli-smoke.sh
```

With `POLKAMETER_ZOMBIENET_KEEP_ARTIFACTS=1`, output remains in `target/zombienet-cli-smoke`; otherwise the script cleans it up on exit.
