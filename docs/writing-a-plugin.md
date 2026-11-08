# Writing a plugin

Let's write a plugin with one operation, `greet`, install it and run a plan that calls it. You don't need a chain for this.

PolkaMeter doesn't know your runtime. Your plugin builds the transactions for your chain, and PolkaMeter sends them and measures what happens. A plugin is a separate program, so you can keep it in your own repository and install it without rebuilding PolkaMeter.

Before you start, [install PolkaMeter](getting-started.md) so `polkameter` is on your `PATH`. You also need Rust 1.93 or newer.

## Create the crate

Run this where you keep your projects:

```sh
cargo new hello-plugin
cd hello-plugin
```

Replace `Cargo.toml` with this file. It adds the SDK, the library a plugin uses to talk to PolkaMeter:

```toml
[package]
name = "hello-plugin"
version = "0.1.0"
edition = "2024"
rust-version = "1.93"

[dependencies]
anyhow = "1"
polkameter-plugin-sdk = "0.3"
serde_json = "1"
tokio = { version = "1", features = ["full"] }
```

## Write the plugin

Replace `src/main.rs` with this file:

```rust
use anyhow::{Result, bail};
use polkameter_plugin_sdk::{Context, Manifest, Operation, PROTOCOL, Plugin, Schema};
use serde_json::{Value, json};

struct Hello;

impl Plugin for Hello {
	fn manifest(&self) -> Manifest {
		Manifest {
			id: "hello".into(),
			version: "0.1.0".into(),
			protocol: PROTOCOL,
			operations: [(
				"greet".into(),
				Operation::new(
					"Greet a name without chain access",
					&[("name", Schema::String)],
					&[("greeting", Schema::String)],
				)
				.read_only(),
			)]
			.into_iter()
			.collect(),
		}
	}

	async fn invoke(
		&mut self,
		operation: &str,
		inputs: Value,
		_context: &Context,
	) -> Result<Value> {
		match operation {
			"greet" => {
				let name = inputs["name"].as_str().unwrap_or_default();
				Ok(json!({"greeting": format!("Hello, {name}!")}))
			},
			_ => bail!("unknown operation {operation}"),
		}
	}
}

#[tokio::main]
async fn main() -> Result<()> {
	polkameter_plugin_sdk::serve(Hello).await
}
```

`manifest` says what the plugin offers. `greet` takes a string, `name`, and returns a string, `greeting`. `invoke` runs the requested operation. `serve` reads requests and writes replies.

Write your logs to standard error, because standard output carries only replies. `.read_only()` marks an operation that changes nothing on a chain, so a plan can use it in `<preflight>`.

The SDK checks every request against the manifest before `invoke` runs. If a plan passes a number for `name`, the plugin replies `value does not match String`, and your code never runs.

## Build and install it

From the `hello-plugin` directory:

```sh
cargo build
polkameter plugin install target/debug/hello-plugin
```

```text
…
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.70s
Installed plugin hello 0.1.0 (1 operation) from ~/hello-plugin/target/debug/hello-plugin
```

Install reads the manifest and records the plugin's ID, version and a hash of the executable `target/debug/hello-plugin`. Install the plugin again after every rebuild. A checksum or version mismatch stops `run` before setup and exits `3`.

## Write a plan that calls it

Save this as `hello.polkameter.xml` in the `hello-plugin` directory:

```xml
<polkameter-plan xmlns="https://polkameter.dev/schema/plan" version="1" name="Hello plugin" timeout-ms="30000">
  <plugins><plugin id="hello" version="0.1.0" protocol="1"/></plugins>
  <setup>
    <step id="greet" use="hello.greet"><input name="name" value="Polkadot"/></step>
    <step id="verified" use="core.assert-equal">
      <input name="actual" ref="steps.greet.greeting"/><input name="expected" value="Hello, Polkadot!"/>
    </step>
  </setup>
</polkameter-plan>
```

- `<plugins>` names the plugin and its version. The version must match the one you installed.
- The first step calls `hello.greet`. Later steps read its output as `steps.greet.greeting`.
- `core.assert-equal` fails the run if `actual` and `expected` differ.

[Plans](plans.md) lists every element.

## Run it

From the `hello-plugin` directory:

```sh
polkameter run hello.polkameter.xml --output runs
```

```text
Phase: preflight
Phase: setup
Phase: teardown
Run completed. Artifacts: ~/hello-plugin/runs/run-1792254908788-10278-1
```

Each run writes a new run directory under `--output`. The setup step ran once, and the workflow step ran once for each user. [Run directory](results.md#the-run-directory) lists the files a run writes.

Now let's break it. Copy the plan to `wrong.polkameter.xml` and change `expected` to `Hello, Ferris!`. Run the copy and print its exit code:

```sh
polkameter run wrong.polkameter.xml --output runs-wrong
echo $?
```

```text
…
Run failed. Artifacts: ~/hello-plugin/runs-wrong/run-1793897634036-94684-1
PolkaMeter: run failed: assertion verified failed: expected "Hello, Ferris!", got "Hello, Polkadot!" (details in ~/hello-plugin/runs-wrong/run-1793897634036-94684-1/execution.json)
1
```

The run stops at the failed assertion. The message quotes string values and shows numbers and booleans bare. This assertion failure exits `1`, and `execution.json` keeps the full error.

## Plugins that drive load

A load plugin prepares the flood and probe transactions before the load. It can also check the chain state afterwards, and record extra series for its own checks. [PreparedTx and the load contract](plugins.md#preparedtx-and-the-load-contract) gives the contract, and [The plugin SDK crate](plugins.md#the-plugin-sdk-crate) shows the `chain` and `observers` features you'll need.

Before you write one, write down the load: the call, the signer, the number of transactions, and what each one needs from the chain. Check the runtime identity (genesis hash, spec version and transaction version) before the load starts, so stale transactions stop the run rather than get sent. Keep secrets in credential profiles, not in the plugin's output.

The [substrate plugin](https://github.com/agustinustheo/polkameter/tree/main/plugins/substrate) is a complete load plugin. The [example plugin](https://github.com/agustinustheo/polkameter/blob/main/plugins/example/src/main.rs) is smaller.

When your plugin is ready, [Run a load test](load-test.md) shows how to run it.
