# Getting started

<!-- v0.3.0: remove after release -->
> **Unreleased:** The published CLI is v0.2.0, and step 1 installs it. The other outputs on this page come from v0.3.0, built from source in step 2. Notes give the v0.2.0 output where it differs.

You will install the CLI, run the example plan and read its results. You need no chain; building the example plugin needs Rust 1.93 or newer.

## 1. Install the CLI

Install the published CLI with Rust 1.93 or newer, then check its version:

<!-- TODO(v0.3.0): lead with the prebuilt installer after the first CLI-archive release is published. -->
```sh
cargo install --locked polkameter
polkameter --version
```

```text
polkameter 0.2.0
```

<!-- v0.3.0: remove after release -->
Prebuilt CLI archives, `install.sh`, binstall metadata and containers are available **from v0.3.0**. The [install guide](install.md) lists their requirements; v0.2.0 has no prebuilt CLI archives.

## 2. Build the example plugin

Clone [the repository](https://github.com/agustinustheo/polkameter) and open the checkout. From its root, build the example plugin with Rust 1.93:

```sh
cargo +1.93.0 build --locked -p polkameter-example-plugin
```

The local build completed with:

```text
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 29.17s
```

On a first build, Cargo prints a `Compiling` line for each crate before the `Finished` line. The plugin executable is `target/debug/polkameter-example-plugin` (`.exe` on Windows). Keep using the repository root for the remaining commands.

<!-- v0.3.0: remove after release (this paragraph and the two blocks below) -->
To use the v0.3.0 CLI before its release, such as for `junit.xml`, build it from this checkout instead:

```sh
cargo +1.93.0 build --locked -p polkameter --bin polkameter
export PATH="$PWD/target/debug:$PATH"
polkameter --version
```

```text
polkameter 0.3.0
```

## 3. Install the example plugin

PolkaMeter records installed plugins in a registry file. Keep it under `target/` for now:

```sh
export POLKAMETER_PLUGIN_REGISTRY="$PWD/target/plugins.json"
polkameter plugin install target/debug/polkameter-example-plugin
```

```text
Installed plugin example 0.1.0 (6 operations) from ~/polkameter/target/debug/polkameter-example-plugin
```

Check what the registry holds:

```sh
polkameter plugin list
```

```text
{
  "capabilities": [],
  "credentials": {},
  "plugins": {
    "example": {
...
      "executable": "~/polkameter/target/debug/polkameter-example-plugin",
      "version": "0.1.0"
    }
  },
  "topologies": {}
}
```

PolkaMeter refuses to run a plugin whose file changed since you installed it. Install it again after rebuilding the plugin.

## 4. Validate and run the example plan

```sh
polkameter validate examples/plugin-workflow.polkameter.xml
```

```text
Plan is structurally valid. Preflight checks the installed plugins.
```

```sh
polkameter run examples/plugin-workflow.polkameter.xml --output target/runs
```

```text
Phase: preflight
Phase: setup
Phase: workflow
Phase: teardown
Run completed. Artifacts: ~/polkameter/target/runs/run-1792254543387-3002-1
```

The setup step calls `example.double` on 21, which returns 42, and `core.echo` copies that value. Each of the 3 users runs 2 iterations of the workflow. In each iteration, `example.double` doubles 42 to 84, and `core.assert-equal` fails the run unless the result is 84.

## 5. Read the run directory

Each run writes to its own directory, named `run-<timestamp>-<pid>-<sequence>`. Replace `<run>` below with the name after `Artifacts:`:

```sh
ls target/runs/<run>
```

```text
events.jsonl
execution.json
junit.xml
plan.json
plots
plugins
resolved-plan.json
samples.jtl
summary.md
```

<!-- v0.3.0: remove after release -->
The v0.2.0 CLI writes the same files except `junit.xml`, which is new in v0.3.0.

```sh
cat target/runs/<run>/summary.md
```

```markdown
# Plugin data flow

Status: completed

All configured steps completed.
```

These four files matter most at first:

| File | What it tells you |
| --- | --- |
| `summary.md` | The status, and the error if the run failed. Start here. |
| `execution.json` | The outcome: `state`, `exit_code` and `error`. |
| `samples.jtl` | One CSV row for each step that ran, in JMeter's format. |
| `events.jsonl` | Every event of the run, one JSON object per line. |

The [run directory](results.md) page describes the other files.

## 6. Break it

Make a copy of the plan that expects the wrong answer, 85 instead of 84:

```sh
sed 's/value="84"/value="85"/' examples/plugin-workflow.polkameter.xml > target/broken.polkameter.xml
```

Run the broken plan and print its exit code:

```sh
polkameter run target/broken.polkameter.xml --output target/runs
echo $?
```

```text
Phase: preflight
Phase: setup
Phase: workflow
Phase: teardown
Run failed. Artifacts: ~/polkameter/target/runs/run-…
PolkaMeter: run failed: assertion verified failed: expected 85, got 84 (details in ~/polkameter/target/runs/run-…/execution.json)
1
```

<!-- v0.3.0: remove after release -->
The v0.2.0 CLI prints a shorter reason, without the values:

```text
Polkameter: run failed: assertion verified failed (details in ~/polkameter/target/runs/run-…/execution.json)
```

The last line before the exit code gives the reason: the step `verified` found a different value. The run directory keeps the same error in `summary.md`:

```sh
cat target/runs/<run>/summary.md
```

```markdown
# Plugin data flow

Status: failed

assertion verified failed: expected 85, got 84
```

<!-- v0.3.0: remove after release -->
With v0.2.0, the last line is `assertion verified failed`.

Exit code `1` means the run ended in the `failed` state. [Exit codes](cli.md#exit-codes) lists every code.

## Next steps

- [Run a load test](load-test.md): point a plan at a chain and measure it.
- [Write a plugin](writing-a-plugin.md): supply the steps your runtime needs.
- [Plans](plans.md): every element of the plan XML.
