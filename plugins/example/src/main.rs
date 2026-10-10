use anyhow::{Result, bail};
use polkameter_plugin_sdk::{Context, Manifest, Operation, PROTOCOL, Plugin, Schema};
use serde_json::{Value, json};
struct Example;
impl Plugin for Example {
	fn manifest(&self) -> Manifest {
		let operations = [
			("crash", Operation::new("Exercise unexpected process exit", &[], &[]).read_only()),
			(
				"invalid-output",
				Operation::new("Exercise output validation", &[], &[("value", Schema::Integer)])
					.read_only(),
			),
			(
				"fail-with-message",
				Operation::new("Exercise error redaction", &[("message", Schema::String)], &[])
					.read_only(),
			),
			(
				"double",
				Operation::new(
					"Double a number without chain access",
					&[("value", Schema::Integer)],
					&[("value", Schema::Integer)],
				)
				.read_only(),
			),
			("fail", Operation::new("Exercise tool-failure handling", &[], &[]).read_only()),
			(
				"wait",
				Operation::new(
					"Exercise cancellation and deadlines",
					&[("milliseconds", Schema::Integer)],
					&[],
				)
				.read_only(),
			),
		];
		Manifest {
			id: "example".into(),
			version: "0.1.0".into(),
			protocol: PROTOCOL,
			requirements: vec![],
			operations: operations
				.into_iter()
				.map(|(name, operation)| (name.into(), operation))
				.collect(),
		}
	}
	async fn invoke(&mut self, operation: &str, inputs: Value, context: &Context) -> Result<Value> {
		match operation {
			"crash" => {
				eprintln!("example plugin intentional crash");
				std::process::exit(42)
			},
			"invalid-output" => Ok(json!({"value":"not an integer"})),
			"fail-with-message" => {
				let message = inputs["message"].as_str().unwrap_or("");
				eprintln!("{message}");
				bail!("{message}")
			},
			"double" => Ok(
				json!({"value": inputs["value"].as_i64().and_then(|n| n.checked_mul(2)).ok_or_else(|| anyhow::anyhow!("integer overflow"))?}),
			),
			"wait" => {
				std::fs::write(
					context.artifact_dir.join("wait.pid"),
					std::process::id().to_string(),
				)?;
				tokio::time::sleep(std::time::Duration::from_millis(
					inputs["milliseconds"].as_u64().unwrap_or(0),
				))
				.await;
				Ok(json!({}))
			},
			_ => bail!("intentional plugin failure"),
		}
	}
}
#[tokio::main]
async fn main() -> Result<()> {
	polkameter_plugin_sdk::serve(Example).await
}
