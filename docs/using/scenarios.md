# XML test plans

Polkameter's portable, human-authored test-plan format is XML, with the filename suffix `.polkameter.xml`. The desktop application saves XML by default and the CLI reads it directly. Keep plans in version control, review them like code, and supply the signer separately at execution time.

The format is versioned (`version="1"`) and has a machine-readable [XSD schema](https://github.com/agustinustheo/polkameter/blob/main/schemas/polkameter-plan-v1.xsd). The [XML plan v1 source](../xml-format-v1.md) is a compact canonical example. JSON plans are still readable for compatibility, but new plans should be XML.

## Complete example

```xml
<?xml version="1.0" encoding="UTF-8"?>
<polkameter-plan xmlns="https://polkameter.dev/schema/plan/v1" version="1">
  <test-plan name="Transfer burst" seed="42">
    <description>A controlled local transfer test</description>
    <limits whole-run-timeout-ms="900000" shutdown-drain-timeout-ms="300000" max-concurrent-samples="100"/>
  </test-plan>
  <chain endpoint="ws://127.0.0.1:9944" transaction-profile="polkadot">
    <prometheus endpoint="http://127.0.0.1:9615/metrics"/>
  </chain>
  <signer profile="local-dev" derivation-path="//polkameter">
    <funding amount="1000000000000" finality-timeout-ms="60000" batch-size="50"/>
  </signer>
  <user-group name="transfer-users" users="10" concurrency="5" iterations="2">
    <arrival kind="ramp" duration-ms="5000"/>
    <workflow>
      <call label="transfer keep alive" pallet="Balances" method="transfer_keep_alive"
            completion="finalized" mortality-period="4096" finality-timeout-ms="300000">
        <arguments>{&quot;dest&quot;:{&quot;$variant&quot;:&quot;Id&quot;,&quot;value&quot;:{&quot;$bytes&quot;:&quot;0xd43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27d&quot;}},&quot;value&quot;:&quot;1000000000000&quot;}</arguments>
        <assertion kind="success"/>
        <assertion kind="max-elapsed" milliseconds="30000"/>
      </call>
    </workflow>
  </user-group>
  <collectors>
    <collector kind="jtl"/>
    <collector kind="events-jsonl"/>
    <collector kind="telemetry-jsonl"/>
    <collector kind="summary"/>
    <collector kind="svg-plots"/>
  </collectors>
</polkameter-plan>
```

XML special characters in the JSON inside `<arguments>` must be escaped. The desktop handles that automatically when saving.

## Plan structure

The root requires the exact namespace `https://polkameter.dev/schema/plan/v1` and version `1`. Its children appear in this order: `test-plan`, `chain`, `signer`, one or more `user-group` elements, then `collectors`.

- **`test-plan`** declares the name, deterministic seed, optional description, and whole-run/drain/concurrency limits.
- **`chain`** declares a `ws://` or `wss://` RPC endpoint, `transaction-profile` (normally `polkadot`), and optionally a Prometheus metrics endpoint.
- **`signer`** supplies only the credential-vault profile alias and derivation path. It has no SURI attribute. Its optional `funding` child is limited to local development chains.
- **`user-group`** defines virtual-user count, group concurrency, iterations, one arrival model, and optional `setup`, `workflow`, and `teardown` call lists.
- **`collectors`** selects evidence to write after a run.

Setup runs once before scheduled users, workflow calls run in their written order for each user/iteration, and teardown runs once after the workload drains.

## Arrival models

Use one `<arrival>` element per group:

| Kind | Required attribute | Behavior |
|---|---|---|
| `burst` | `window-ms` | Seeded random starts inside the window. |
| `ramp` | `duration-ms` | Evenly spreads starts from zero through the duration. |
| `poisson` | `rate-per-second` | Uses deterministic exponential inter-arrival gaps. |

Each duration and rate must be positive. The scheduler is deterministic for a fixed plan seed.

## Dynamic call arguments

Call arguments stay JSON inside the XML `<arguments>` element because each target runtime defines its own SCALE shape. The desktop can fetch live runtime metadata and present normal labelled inputs for a selected pallet/call; the advanced raw JSON editor remains available for complex types.

The dynamic converter uses these conventions:

- Decimal strings such as `"1000000000000"` become unsigned integers. Use strings for balances beyond JavaScript-safe integer range.
- `{ "$bytes": "0x..." }` represents bytes and must include `0x`.
- `{ "$variant": "VariantName", "value": ... }` represents an enum variant.
- JSON `null` represents the `None` variant. Floating-point values are rejected.

Preflight against live metadata is the final authority: it confirms the pallet/call exists and the arguments SCALE-encode for the actual chain.

## Validate in CI

Use both the XSD and CLI validator when a CI runner has `xmllint`:

```sh
xmllint --noout \
  --schema schemas/polkameter-plan-v1.xsd \
  scenarios/transfer.polkameter.xml

polkameter validate scenarios/transfer.polkameter.xml --format json
```

The XSD verifies XML structure; `polkameter validate` adds the runner's semantic constraints. `preflight` then adds live metadata and signer readiness.
