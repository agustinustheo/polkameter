# Getting started

You will build the CLI, run the example plan and read its results. You need no chain.

## 1. Check your Rust version

PolkaMeter needs Rust 1.93 or newer:

```sh
rustc --version
```

```text
rustc 1.92.0 (ded5c06cf 2025-12-08)
```

With Rust 1.92, the build stops before it compiles anything:

```sh
cargo build -p polkameter --bin polkameter
```

```text
error: rustc 1.92.0 is not supported by the following packages:
  polkameter@0.2.0 requires rustc 1.93
  polkameter@0.2.0 requires rustc 1.93
  polkameter-chain@0.2.0 requires rustc 1.93
  polkameter-checks@0.2.0 requires rustc 1.93
  polkameter-engine@0.2.0 requires rustc 1.93
  polkameter-files@0.2.0 requires rustc 1.93
  polkameter-load@0.2.0 requires rustc 1.93
  polkameter-monitors@0.2.0 requires rustc 1.93
  polkameter-plugin-sdk@0.2.0 requires rustc 1.93
```

Run `rustup update` to get the latest stable Rust, then check the version again with `rustc --version`.

## 2. Build the CLI and the example plugin

Clone [the repository](https://github.com/agustinustheo/polkameter) and open the checkout. Run both commands from its root:

```sh
cargo build -p polkameter --bin polkameter
cargo build -p polkameter-example-plugin
```

```text
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.22s
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.09s
```

On a first build, cargo prints a `Compiling` line for each crate before the `Finished` line. The builds produce `target/debug/polkameter` and `target/debug/polkameter-example-plugin`. Put the first one on your `PATH` for this shell:

```sh
export PATH="$PWD/target/debug:$PATH"
which polkameter
```

```text
~/polkameter/target/debug/polkameter
```

The `export` lasts only for the current shell. Run it again in a new terminal.

To put `polkameter` in `~/.cargo/bin` instead, run `cargo install --locked --git https://github.com/agustinustheo/polkameter polkameter`. You still need the checkout for the example plugin and plan.

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
  "example": {
...
    "executable": "~/polkameter/target/debug/polkameter-example-plugin",
    "version": "0.1.0"
  }
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
plan.json
plots
plugins
resolved-plan.json
samples.jtl
summary.md
```

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
Polkameter: run failed: assertion verified failed (details in ~/polkameter/target/runs/run-…/execution.json)
1
```

The last line before the exit code gives the reason: the step `verified` found a different value. The run directory keeps the same error in `summary.md`:

```sh
cat target/runs/<run>/summary.md
```

```markdown
# Plugin data flow

Status: failed

assertion verified failed
```

Exit code `1` means the run ended in the `failed` state. [Exit codes](cli.md#exit-codes) lists every code.

## Next steps

- [Run a load test](load-test.md): point a plan at a chain and measure it.
- [Write a plugin](writing-a-plugin.md): supply the steps your runtime needs.
- [Plans](plans.md): every element of the plan XML.
