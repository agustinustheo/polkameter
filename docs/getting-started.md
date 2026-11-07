# Getting started

Let's get you from nothing to a finished run. You'll build the CLI, run the example plan, and read what it wrote. You don't need a chain for any of this. You do need Rust 1.93 or newer.

## 1. Build the CLI and the example plugin

Clone [the repository](https://github.com/agustinustheo/polkameter) and run every command below from its root:

```sh
cargo +1.93.0 build --locked -p polkameter -p polkameter-example-plugin
export PATH="$PWD/target/debug:$PATH"
polkameter --version
```

```text
polkameter 0.3.0
```

The example plugin is `target/debug/polkameter-example-plugin`, with `.exe` on Windows. If you already [installed](install.md) PolkaMeter, you still need this plugin build.

## 2. Install the example plugin

A plugin is a separate program that gives the plan its operations. PolkaMeter records installed plugins in a registry file. Keep it under `target/` for now:

```sh
export POLKAMETER_PLUGIN_REGISTRY="$PWD/target/plugins.json"
polkameter plugin install target/debug/polkameter-example-plugin
```

```text
Installed plugin example 0.1.0 (7 operations) from ~/polkameter/target/debug/polkameter-example-plugin
```

PolkaMeter refuses to run a plugin whose file changed after you installed it. Install it again after each rebuild.

## 3. Validate and run the example plan

```sh
polkameter validate examples/plugin-workflow.polkameter.xml
polkameter run examples/plugin-workflow.polkameter.xml --output target/runs
```

```text
Plan is structurally valid. Preflight checks the installed plugins.
Phase: preflight
Phase: setup
Phase: workflow
Phase: teardown
Run completed. Artifacts: ~/polkameter/target/runs/run-1792254543387-3002-1
```

Here's what the plan did. The setup step calls `example.double` on 21 and gets 42. Then the workflow runs. The plan simulates three users, and each user repeats the steps twice. Each time, `example.double` turns 42 into 84. Then `core.assert-equal` checks the result. It's an assertion: a step that fails the run when two values differ.

## 4. Read the run directory

Each run gets its own directory, named `run-<timestamp>-<pid>-<sequence>`. Replace `<run>` with the name after `Artifacts:`:

```sh
cat target/runs/<run>/summary.md
```

```markdown
# Plugin data flow

Status: completed

All configured steps completed.
```

Start with `summary.md`. The other files you'll use most:

- `execution.json` says how the run ended: `state`, `exit_code` and `error`.
- `samples.jtl` has one CSV row per step. The columns follow JMeter, a load-testing tool for websites, so JMeter's tools can chart it.
- `junit.xml` is a test report that CI systems can display.
- `events.jsonl` has every event of the run, one JSON object per line.

[Run directory](results.md) describes every file.

## 5. Break it

Make a copy of the plan that expects the wrong answer, 85 instead of 84. Run it and print the exit code:

```sh
sed 's/value="84"/value="85"/' examples/plugin-workflow.polkameter.xml > target/broken.polkameter.xml
polkameter run target/broken.polkameter.xml --output target/runs
echo $?
```

```text
…
Run failed. Artifacts: ~/polkameter/target/runs/run-…
PolkaMeter: run failed: assertion verified failed: expected 85, got 84 (details in ~/polkameter/target/runs/run-…/execution.json)
1
```

The last line before the exit code gives the reason. The step `verified` got 84, not 85. `summary.md` in the new run directory shows the same error. This assertion failure exits `1`. [Exit codes](cli.md#exit-codes) lists every code.

That's it. You've run a plan, read its results and seen a failure. Next, [run a load test](load-test.md) against a chain, or [stress-test a local dev chain](dev-chain.md).
