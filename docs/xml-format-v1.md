# Polkameter XML plan v1

Polkameter plans are portable XML files ending in `.polkameter.xml`. They are
the file used by both the desktop app and headless CLI. The authoritative
schema is [`polkameter-plan-v1.xsd`](https://github.com/agustinustheo/polkameter/blob/main/schemas/polkameter-plan-v1.xsd).

```xml
<?xml version="1.0" encoding="UTF-8"?>
<polkameter-plan xmlns="https://polkameter.dev/schema/plan/v1" version="1">
  <test-plan name="Transfer journey" seed="1">
    <description>Simulate an ordinary transfer.</description>
    <limits whole-run-timeout-ms="900000" shutdown-drain-timeout-ms="300000" max-concurrent-samples="1000"/>
  </test-plan>
  <chain endpoint="ws://127.0.0.1:9944" transaction-profile="polkadot"/>
  <signer profile="local-dev" derivation-path="//polkameter"/>
  <user-group name="Buyers" users="100" concurrency="20" iterations="1">
    <arrival kind="ramp" duration-ms="30000"/>
    <workflow>
      <call label="transfer" pallet="Balances" method="transfer_keep_alive" completion="finalized" mortality-period="4096" finality-timeout-ms="300000">
        <arguments>{"dest":{"$variant":"Id","value":{"$bytes":"0xd43593c715fdd31c61141abd04a99fd6822c8558854ccde39a5684e7a56da27d"}},"value":"1000000000000"}</arguments>
        <assertion kind="success"/>
      </call>
    </workflow>
  </user-group>
  <collectors><collector kind="jtl"/><collector kind="summary"/></collectors>
</polkameter-plan>
```

`<workflow>` calls run in their written order for each virtual user. `<setup>`
runs once before scheduled users and `<teardown>` runs once after load drains.
Call arguments remain JSON because the selected chain's runtime metadata defines
their shape; the desktop editor can fetch the metadata and normally presents
them as labelled fields, with an advanced raw JSON escape hatch for complex values.

Plans never contain a SURI. Use a signer profile in the desktop credential vault
or `--signer-env` in CI.
