# PolkaMeter

PolkaMeter runs load and stress tests against blockchains built with the Polkadot SDK, the way JMeter runs tests against web services. You describe the work in an XML plan, and PolkaMeter runs it and writes the results to a run directory.

The example plan, trimmed to its core:

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

Run the full example, `examples/plugin-workflow.polkameter.xml`, with:

```sh
polkameter run examples/plugin-workflow.polkameter.xml --output target/runs
```

A run needs three things from you:

- **A plan**: the XML file. It lists the steps, how many users run them and what to check.
- **Plugins**: programs that supply the steps that depend on the runtime, such as building a transaction.
- **A chain**: the network under test. The example needs none.

## Where to start

- [Getting started](getting-started.md): build the CLI, install the example plugin and run the example plan.
- [Run a load test](load-test.md): run a plan against a chain and read the verdict.
- [Run PolkaMeter in CI](ci.md): run plans in a pipeline and check the exit code.
- [Plans](plans.md): every element of the plan XML.
