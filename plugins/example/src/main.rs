use anyhow::{Result, bail};
use polkameter_plugin_sdk::{Context, Manifest, Operation, PROTOCOL, Plugin, Schema};
use serde_json::{Value, json};
struct Example;
impl Plugin for Example {
	fn manifest(&self) -> Manifest {
		Manifest {
			id: "example".into(),
			version: "0.1.0".into(),
			protocol: PROTOCOL,
			requirements: vec![],
			operations: [
				(
					"crash".into(),
					Operation {
						description: "Exercise unexpected process exit".into(),
						inputs: [].into(),
						outputs: [].into(),
						read_only: true,
					},
				),
				(
					"invalid-output".into(),
					Operation {
						description: "Exercise output validation".into(),
						inputs: [].into(),
						outputs: [("value".into(), Schema::Integer.into())].into(),
						read_only: true,
					},
				),
				(
					"fail-with-message".into(),
					Operation {
						description: "Exercise error redaction".into(),
						inputs: [("message".into(), Schema::String.into())].into(),
						outputs: [].into(),
						read_only: true,
					},
				),
				(
					"double".into(),
					Operation {
						description: "Double a number without chain access".into(),
						inputs: [("value".into(), Schema::Integer.into())].into(),
						outputs: [("value".into(), Schema::Integer.into())].into(),
						read_only: true,
					},
				),
				(
					"fail".into(),
					Operation {
						description: "Exercise tool-failure handling".into(),
						inputs: [].into(),
						outputs: [].into(),
						read_only: true,
					},
				),
				(
					"wait".into(),
					Operation {
						description: "Exercise cancellation and deadlines".into(),
						inputs: [("milliseconds".into(), Schema::Integer.into())].into(),
						outputs: [].into(),
						read_only: true,
					},
				),
			]
			.into(),
		}
	}
	async fn invoke(&mut self, operation: &str, inputs: Value, context: &Context) -> Result<Value> {
		match operation {
			"crash" => {
				eprintln!("example plugin intentional crash");
				std::process::exit(42)
			},
			"invalid-output" => Ok(json!({"value":"not an integer"})),
			"fail-with-message" => bail!("{}", inputs["message"].as_str().unwrap_or("")),
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
