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
			"context": {"run_id": "run-test", "artifact_dir": std::env::temp_dir(), "user": null, "iteration": null},
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
	// The SDK checks types and unknown inputs against the manifest.
	assert_eq!(
		session.error("prepare-remarks", json!({"target": target, "seed": "//Alice", "count": 10})),
		"missing field probes"
	);
	assert_eq!(
		session.error(
			"prepare-remarks",
			json!({"target": target, "seed": "//Alice", "count": 10, "probes": 1, "flood-signer": 1})
		),
		"unknown field flood-signer"
	);
	// The plugin checks ranges and formats, and never echoes a secret.
	assert_eq!(
		session.error(
			"prepare-remarks",
			json!({"target": target, "seed": "//Alice", "count": 0, "probes": 1})
		),
		"input count must be between 1 and 1000000, got 0"
	);
	let error = session.error(
		"prepare-transfers",
		json!({"target": target, "seed": "correct horse battery//1", "count": 20, "probes": 1}),
	);
	assert!(error.starts_with("input seed is not a valid secret URI"), "{error}");
	assert!(!error.contains("horse"), "{error}");
	assert_eq!(
		session.error(
			"fund-accounts",
			json!({"target": "http://127.0.0.1:1", "seed": "//Alice", "amount": 1})
		),
		"input target must be a ws:// or wss:// endpoint, got \"http://127.0.0.1:1\""
	);
	assert_eq!(
		session
			.error("fund-accounts", json!({"target": target, "seed": "//Alice", "amount": "1.5"})),
		"input amount must be digits, got \"1.5\""
	);
	assert_eq!(
		session.error("preflight", json!({"target": target, "min-balance": 5})),
		"input min-balance needs input seed"
	);
}

#[test]
fn an_unreachable_chain_is_named() {
	let mut session = Session::start();
	let error = session.error(
		"prepare-remarks",
		json!({"target": "ws://127.0.0.1:1", "seed": "//Alice", "count": 20, "probes": 2}),
	);
	assert!(error.starts_with("cannot connect to ws://127.0.0.1:1"), "{error}");
	let error = session.error("preflight", json!({"target": "ws://127.0.0.1:1"}));
	assert!(error.starts_with("cannot connect to ws://127.0.0.1:1"), "{error}");
}

#[tokio::test]
async fn polkameter_installs_it_and_accepts_the_example_plans() {
	let mut registry = polkameter::engine::plugins::Registry::default();
	registry.install(std::path::Path::new(PLUGIN)).await.unwrap();
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
	// The Agent Skill bundles copies of the example plans, under the same names; they must not
	// drift. tests/validate-plans.py checks the same.
	let templates = concat!(env!("CARGO_MANIFEST_DIR"), "/../../skills/polkameter/templates");
	let mut copies = 0;
	for entry in std::fs::read_dir(templates).unwrap() {
		let path = entry.unwrap().path();
		let name = path.file_name().unwrap().to_string_lossy().into_owned();
		if !name.ends_with(".polkameter.xml") {
			continue;
		}
		copies += 1;
		let example = std::fs::read_to_string(format!("{examples}/{name}"))
			.unwrap_or_else(|e| panic!("examples/{name}: {e}"));
		assert_eq!(
			std::fs::read_to_string(&path).unwrap(),
			example,
			"skills/polkameter/templates/{name} differs from examples/{name}"
		);
	}
	assert_eq!(copies, 3, "the skill holds three plan templates");
}
