//! The plugin executable over its JSON-line protocol, without a chain: the manifest, input
//! checks and connection errors, then the same manifest through PolkaMeter's own registry.

use std::{
	io::{BufRead, BufReader, Write},
	process::{Child, ChildStdin, ChildStdout, Command, Stdio},
};

use serde_json::{Value, json};

const PLUGIN: &str = env!("CARGO_BIN_EXE_polkameter-substrate-plugin");

struct Session {
	child: Child,
	stdin: ChildStdin,
	stdout: BufReader<ChildStdout>,
	next: u64,
}

impl Session {
	fn start() -> Self {
		let mut child = Command::new(PLUGIN)
			.stdin(Stdio::piped())
			.stdout(Stdio::piped())
			.stderr(Stdio::null())
			.spawn()
			.unwrap();
		let stdin = child.stdin.take().unwrap();
		let stdout = BufReader::new(child.stdout.take().unwrap());
		Self { child, stdin, stdout, next: 0 }
	}

	fn call(&mut self, operation: &str, inputs: Value) -> Value {
		self.next += 1;
		let request = json!({
			"protocol": 1,
			"id": self.next,
			"operation": operation,
			"inputs": inputs,
			"context": {"run_id": "run-test", "artifact_dir": std::env::temp_dir()},
		});
		writeln!(self.stdin, "{request}").unwrap();
		self.stdin.flush().unwrap();
		let mut line = String::new();
		self.stdout.read_line(&mut line).unwrap();
		let reply: Value = serde_json::from_str(&line).unwrap();
		assert_eq!(reply["id"], self.next);
		reply
	}

	fn error(&mut self, operation: &str, inputs: Value) -> String {
		let reply = self.call(operation, inputs);
		assert!(reply["result"].is_null(), "{reply}");
		reply["error"].as_str().unwrap().to_owned()
	}
}

impl Drop for Session {
	fn drop(&mut self) {
		let _ = self.call("$shutdown", json!({}));
		let _ = self.child.wait();
	}
}

#[test]
fn describe_lists_the_operations() {
	let mut session = Session::start();
	let manifest = session.call("$describe", json!({}))["result"].clone();
	assert_eq!(manifest["id"], "substrate");
	assert_eq!(manifest["protocol"], 1);
	let operations: Vec<&str> =
		manifest["operations"].as_object().unwrap().keys().map(String::as_str).collect();
	assert_eq!(
		operations,
		["check-state", "fund-accounts", "preflight", "prepare-remarks", "prepare-transfers"]
	);
	assert_eq!(manifest["operations"]["preflight"]["read_only"], true);
	assert_eq!(manifest["operations"]["prepare-remarks"]["inputs"]["seed"]["optional"], false);
}

#[test]
fn bad_inputs_fail_before_any_connection() {
	let mut session = Session::start();
	let target = "ws://127.0.0.1:1";
	// Each case is refused, and the error names the input at fault. The SDK checks types and
	// unknown inputs against the manifest; the plugin checks ranges and formats.
	for (operation, inputs, field) in [
		("prepare-remarks", json!({"target": target, "seed": "//Alice", "count": 10}), "probes"),
		(
			"prepare-remarks",
			json!({"target": target, "seed": "//Alice", "count": 10, "probes": 1, "flood-signer": 1}),
			"flood-signer",
		),
		(
			"prepare-remarks",
			json!({"target": target, "seed": "//Alice", "count": 0, "probes": 1}),
			"count",
		),
		(
			"prepare-transfers",
			json!({"target": target, "seed": "correct horse battery//1", "count": 20, "probes": 1}),
			"seed",
		),
		(
			"fund-accounts",
			json!({"target": "http://127.0.0.1:1", "seed": "//Alice", "amount": 1}),
			"target",
		),
		("fund-accounts", json!({"target": target, "seed": "//Alice", "amount": "1.5"}), "amount"),
		("preflight", json!({"target": target, "min-balance": 5}), "min-balance"),
	] {
		let error = session.error(operation, inputs);
		assert!(error.contains(field), "{operation} {field}: {error}");
		// A secret is never echoed.
		assert!(!error.contains("horse"), "{error}");
	}
	// Good inputs against an unreachable chain fail and name the endpoint.
	for (operation, inputs) in [
		("prepare-remarks", json!({"target": target, "seed": "//Alice", "count": 20, "probes": 2})),
		("preflight", json!({"target": target})),
	] {
		let error = session.error(operation, inputs);
		assert!(error.contains(target), "{operation}: {error}");
	}
}

#[tokio::test]
async fn polkameter_installs_it_and_accepts_the_example_plans() {
	polkameter::engine::plugins::Registry::describe(std::path::Path::new(PLUGIN))
		.await
		.unwrap();
	let examples = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples");
	for name in [
		"substrate-remarks",
		"substrate-remarks-ci",
		"substrate-transfers",
		"substrate-transfers-ci",
	] {
		let xml = std::fs::read_to_string(format!("{examples}/{name}.polkameter.xml")).unwrap();
		let plan = polkameter::engine::plan::Plan::parse(&xml).unwrap();
		let plugin = &plan.plugins.entries[0];
		assert_eq!(plugin.id, "substrate", "{name}");
		assert_eq!(plugin.version, env!("CARGO_PKG_VERSION"), "{name}: plugin version");
	}
}
