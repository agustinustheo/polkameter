# Scenarios

The `.polkameter.json` scenario is the authoritative, portable test-plan format. Save it in version control, review it like code, and provide the secret independently at execution time. The current format version is `1`; version `0` migrates to `1`, while newer versions are rejected.

## Complete example

```json
{
  "version": 1,
  "testPlan": {
    "name": "Transfer burst",
    "description": "A controlled local transfer test",
    "seed": 42,
    "limits": {
      "wholeRunTimeoutMs": 900000,
      "shutdownDrainTimeoutMs": 300000,
      "maxConcurrentSamples": 100
    }
  },
  "chain": {
    "endpoint": "ws://127.0.0.1:9944",
    "prometheusEndpoint": "http://127.0.0.1:9615/metrics",
    "transactionProfile": "polkadot"
  },
  "signerSource": {
    "profile": "local-dev",
    "baseSuri": "[redacted]",
    "derivationPath": "//polkameter",
    "funding": {
      "amount": "1000000000000",
      "finalityTimeoutMs": 60000,
      "batchSize": 50
    }
  },
  "threadGroups": [
    {
      "name": "transfer-users",
      "users": 10,
      "concurrency": 5,
      "iterations": 2,
      "arrival": { "kind": "ramp", "durationMs": 5000 },
      "samplers": [
        {
          "phase": "transaction",
          "label": "transfer keep alive",
          "pallet": "Balances",
          "call": "transfer_keep_alive",
          "arguments": {
            "dest": { "$variant": "Id", "value": { "$bytes": "0xd43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27d" } },
            "value": "1000000000000"
          },
          "completion": "finalized",
          "mortalityPeriod": 4096,
          "finalityTimeoutMs": 300000,
          "assertions": [
            { "kind": "success" },
            { "kind": "max_elapsed", "milliseconds": 30000 }
          ]
        }
      ]
    }
  ],
  "collectors": ["jtl", "events_jsonl", "telemetry_jsonl", "summary", "svg_plots"]
}
```

## Validation rules

Structural validation requires at least one thread group and sampler; non-empty plan, group, sampler, pallet, and call names; `ws://` or `wss://` RPC endpoints; a positive user count; and concurrency between one and the group user count. Timeouts must be at least 1,000 ms. Mortality periods must be powers of two and at least four.

The plan-wide timeout, drain timeout, and maximum concurrent sample count must all be positive. A Prometheus endpoint, when present, must use `http://` or `https://`. The complete field-level reference is in [Scenario schema](../reference/scenario-schema.md).

## Dynamic call arguments

Arguments must be a JSON object or array and are dynamically converted to SCALE values against the target runtime metadata. Plain JSON has two deliberate conventions:

- Decimal strings such as `"1000000000000"` become unsigned integers. Use strings for balances that exceed JavaScript-safe integer range.
- `{ "$bytes": "0x..." }` represents bytes and must include the `0x` prefix.
- `{ "$variant": "VariantName", "value": ... }` represents an enum variant. The optional value becomes the variant's field(s).
- JSON `null` represents the `None` variant. Floating-point values are not accepted; use integers or decimal strings.

Preflight is the source of truth for whether a call exists and its arguments encode against the chain you actually target.

## Collectors

Collectors describe the desired evidence: `jtl`, `events_jsonl`, `telemetry_jsonl`, `summary`, and `svg_plots`. The current runner produces the portable artifact contract described in [Artifacts and reports](artifacts.md); retain all collectors for a complete operational record.
