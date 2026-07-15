# Your first local run

Run this only against a chain you are willing to load. A local `--dev --tmp` node is ideal because it starts with development accounts and disappears when stopped.

## 1. Start a local node

```sh
polkadot --dev --tmp \
  --rpc-port 9944 \
  --prometheus-port 9615 \
  --rpc-methods Unsafe \
  --rpc-cors all
```

Keep this process running. Its WebSocket endpoint is `ws://127.0.0.1:9944`; Prometheus is available at `http://127.0.0.1:9615/metrics`.

## 2. Copy or write a scenario

The repository's structural fixture is a safe starting point:

```sh
cp src-tauri/tests/fixtures/valid-scenario.polkameter.json transfer.polkameter.json
```

It is deliberately redacted: the `signerSource.baseSuri` field is `[redacted]`. Do not replace it with a secret. The CLI refuses to load a scenario containing a literal SURI.

## 3. Validate before connecting

```sh
polkameter validate transfer.polkameter.json
```

This checks the JSON shape and local constraints only. A valid result does not prove that the selected pallet or call exists on the chain.

## 4. Preflight against the node

```sh
POLKAMETER_SURI='//Alice' \
  polkameter preflight transfer.polkameter.json \
  --signer-env POLKAMETER_SURI
```

Preflight reads current runtime metadata, tries to SCALE-encode each sampler, derives a preview of virtual user accounts, and checks signer readiness without submitting. Correct the scenario if any selected call is not encodable.

## 5. Execute and inspect artifacts

```sh
POLKAMETER_SURI='//Alice' \
  polkameter run transfer.polkameter.json \
  --signer-env POLKAMETER_SURI \
  --output target/runs

polkameter report target/runs/run-REPLACE_WITH_RUN_ID
```

The run creates a unique `run-<timestamp>` directory beneath `target/runs`. The final CLI output identifies it. Use [Artifacts and reports](using/artifacts.md) to interpret the bundle.

If the scenario has multiple virtual users, each derived user must be funded before transaction submission. The optional development funding helper is restricted to loopback development endpoints; see [Signers and security](using/signers.md).
