//! A minimal PolkaMeter plugin with one operation, `greet`.
//!
//! Build and install it from a checkout of the PolkaMeter repository:
//!
//! ```sh
//! cargo build -p polkameter-plugin-sdk --example hello
//! polkameter plugin install target/debug/examples/hello
//! ```

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
