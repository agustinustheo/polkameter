# PolkaMeter

PolkaMeter sends transactions to a Polkadot SDK chain at rates you choose. It measures how the chain responds and reports where it stops keeping up.

A plan lists the steps and the checks. Here's the example plan, trimmed:

```xml
<polkameter-plan xmlns="https://polkameter.dev/schema/plan" version="1" name="Plugin data flow">
  <plugins><plugin id="example" version="0.1.0" protocol="1"/></plugins>
  <setup>
    <step id="seed" use="example.double"><input name="value" value="21"/></step>
  </setup>
  <workflow id="users" users="3" iterations="2" concurrency="2">
    <step id="computed" use="example.double"><input name="value" ref="steps.seed.value"/></step>
    <step id="verified" use="core.assert-equal">
      <input name="actual" ref="steps.computed.value"/><input name="expected" value="84"/>
    </step>
  </workflow>
</polkameter-plan>
```

Run the full plan, `examples/plugin-workflow.polkameter.xml`, from a checkout of the repository. Each run writes its reports to a new run directory:

```sh
polkameter run examples/plugin-workflow.polkameter.xml --output target/runs
```

Plugins are programs that supply the steps that depend on your runtime, such as building a transaction. The [substrate plugin](substrate-plugin.md#limits) covers compatible runtimes with the standard `System` and `Balances` pallets. For other runtimes, you [write a plugin](writing-a-plugin.md).

New here? Start with [Getting started](getting-started.md). It needs no chain. Then [stress-test a local dev chain](dev-chain.md) and [run it in CI](ci.md).
