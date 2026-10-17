# Writing a plugin

You will write a plugin with one operation, `greet`, install it and run a plan that calls it. The plan needs no chain.

PolkaMeter does not know your runtime. It submits transactions at a controlled rate, follows blocks and writes the report. A plugin supplies what depends on your chain, such as how to build a transaction or what state a load should leave behind. A plugin is a separate process, so you can keep it in your own repository and install it without rebuilding PolkaMeter.

Before you start, [install PolkaMeter](getting-started.md) so that `polkameter` is on your `PATH`. You also need Rust 1.93 or newer.

## Create the crate

Run this in the directory where you keep your projects:

```sh
cargo new hello-plugin
cd hello-plugin
```

```text
    Creating binary (application) `hello-plugin` package
note: see more `Cargo.toml` keys and their definitions at https://doc.rust-lang.org/cargo/reference/manifest.html
```

Replace `Cargo.toml` with this file. It adds the SDK, the library that lets a plugin talk to PolkaMeter:

```toml
[package]
name = "hello-plugin"
version = "0.1.0"
edition = "2024"
rust-version = "1.93"

[dependencies]
anyhow = "1"
polkameter-plugin-sdk = { git = "https://github.com/agustinustheo/polkameter", rev = "931ef45a36f4238d66e4b7af21b9a2ea20a4caec" }
serde_json = "1"
tokio = { version = "1", features = ["full"] }
```

The SDK is not on crates.io yet, so you take it from git. The `rev` pins a commit. Without it, Cargo follows the tip of `main`, so a fresh build can pick up SDK changes you have not tested.

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
			requirements: vec![],
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

`manifest` declares what the plugin offers. The `greet` operation takes a string input, `name`, and returns a string output, `greeting`. `invoke` runs an operation by name, and `main` hands the plugin to `serve`. `serve` reads requests from standard input and writes replies to standard output.

Write logs to standard error. Standard output carries only protocol replies. `.read_only()` marks an operation that changes nothing on a chain and submits nothing, so a plan may use it in a `<preflight>` step.

The SDK checks every request against the schemas in the manifest before `invoke` runs. If a plan passes a number for `name`, the plugin replies with the error `value does not match String`, and your code never runs.

## Build it

From the `hello-plugin` directory:

```sh
cargo build
```

```text
...
   Compiling polkameter-plugin-sdk v0.2.0 (https://github.com/agustinustheo/polkameter?rev=931ef45a36f4238d66e4b7af21b9a2ea20a4caec#931ef45a)
   Compiling hello-plugin v0.1.0 (~/hello-plugin)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 8.18s
```

The executable is `target/debug/hello-plugin`.

## Install it

```sh
polkameter plugin install target/debug/hello-plugin
```

```text
Installed plugin hello 0.1.0 (1 operation) from ~/hello-plugin/target/debug/hello-plugin
```

Install reads the manifest and records the plugin's ID, its version and the BLAKE2 hash of the executable. PolkaMeter refuses to run a plugin whose executable has changed since you installed it. Install the plugin again after every rebuild. Otherwise the run stops with `plugin hello version/checksum mismatch`.

## Write a plan that calls it

Save this as `hello.polkameter.xml` in the `hello-plugin` directory:

```xml
<polkameter-plan xmlns="https://polkameter.dev/schema/plan" version="1" name="Hello plugin" timeout-ms="30000">
  <plugins><plugin id="hello" version="0.1.0" protocol="1"/></plugins>
  <setup>
    <step id="greet" use="hello.greet"><input name="name" value="Polkadot"/></step>
  </setup>
  <workflow id="check" users="2" iterations="1">
    <step id="verified" use="core.assert-equal">
      <input name="actual" ref="steps.greet.greeting"/><input name="expected" value="Hello, Polkadot!"/>
    </step>
  </workflow>
</polkameter-plan>
```

- `<plugins>` names the plugin and the version the plan uses. The version must match the one you installed. [Plans](plans.md) lists every element.
- The setup step calls `hello.greet` once. Later steps read its output as `steps.greet.greeting`.
- The workflow runs two users. Each user gets a copy of the setup outputs, and `core.assert-equal` fails the run if `actual` and `expected` differ.

## Run it

```sh
polkameter run hello.polkameter.xml --output runs
```

```text
Phase: preflight
Phase: setup
Phase: workflow
Phase: teardown
Run completed. Artifacts: ~/hello-plugin/runs/run-1792254908788-10278-1
```

Each run writes a run directory under `--output`. Its `samples.jtl` file has one row per step and per user:

```sh
cat runs/run-*/samples.jtl
```

```text
timeStamp,elapsed,label,responseCode,responseMessage,threadName,success,bytes,sentBytes,Latency,Connect,allThreads,grpThreads
1792254908830,1,step:greet,STEP_OK,,user:null:iteration:null,true,0,0,0,0,1,1
1792254908831,1,step:verified,STEP_OK,,user:0:iteration:0,true,0,0,0,0,1,1
1792254908832,1,step:verified,STEP_OK,,user:1:iteration:0,true,0,0,0,0,1,1
```

`greet` ran once, in setup. `verified` ran once for each user.

To see a failing check, change `expected` to `Hello, Ferris!` in a copy of the plan, and run that copy with `--output runs-wrong`. The run stops with `assertion verified failed` and exits with code `1`.

## Plugins that drive load

A plugin that drives load does three jobs. The [plugin reference](plugins.md) gives the contract for each.

**Prepared transactions.** Before the load starts, an operation builds the transactions it will send. It returns them as `PreparedTx` values, each holding the encoded bytes, their BLAKE2 hash and any metadata. Return the flood and the probes as two lists that share no transaction. A reply is capped at 8 MiB, so write a long list with `Artifact::write` and return a reference to it.

**State check (optional).** After the run, PolkaMeter samples finalized transactions and calls the operation named by `state-check`. That operation confirms the state the load should leave behind, and returns `checked`, `missing` and `detail`.

**Observers (optional).** A setup step can start a recorder for runtime-specific storage or events, and an evaluate step stops it. The recorder writes its series into the plugin's directory. The evaluate step returns checks that judge those series in the report.

Before you write one, write down the load: the call, the signer, the number of transactions, and what each one needs from the chain. Check the runtime identity (genesis hash, spec version and transaction version) before the load starts, so stale transactions stop the run rather than get submitted. Keep secrets in credential profiles, not in the plugin's output.

For a complete example, read the People plugin in [`stress/people-plugin`](https://github.com/paritytech/polkadot-pop-e2e/tree/feat/th-polkameter-stress-flood/stress/people-plugin) in `paritytech/polkadot-pop-e2e`. It prepares claims, checks their state and observes the Recycler. Its plans are in [`stress/plans`](https://github.com/paritytech/polkadot-pop-e2e/tree/feat/th-polkameter-stress-flood/stress/plans) on the same branch. The repository's own [example plugin](https://github.com/agustinustheo/polkameter/blob/main/plugins/example/src/main.rs) is smaller and exists to exercise failure paths.

## Next steps

The [plugin reference](plugins.md) lists the manifest fields, the protocol and the registry commands. [Plans](plans.md) describes the plan elements that call your operations. When your plugin is ready for a load, [Run a load test](load-test.md) shows how to run it.
