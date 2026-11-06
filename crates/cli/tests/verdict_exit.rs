//! Exercises the CLI process and remote option/exit-code plumbing with a controlled agent, and the
//! verdict a monitorless run's report reaches.
use axum::{
	Json, Router,
	routing::{get, post},
};
use serde_json::{Value, json};
use std::{
	fs,
	path::{Path, PathBuf},
	sync::{
		Arc,
		atomic::{AtomicBool, Ordering},
	},
};

/// `(--fail-on-verdict, --fail-on-inconclusive)`: every combination.
const POLICIES: [(bool, bool); 4] = [(false, false), (true, false), (false, true), (true, true)];
const VERDICTS: [&str; 4] = ["PASS", "WARN", "FAIL", "INCONCLUSIVE"];

/// The exit code the engine's policy gives a verdict (the engine's own tests cover the rules).
fn expected_code((fail_on_verdict, fail_on_inconclusive): (bool, bool), verdict: &str) -> i32 {
	i32::from(
		(verdict == "FAIL" && fail_on_verdict)
			|| (verdict == "INCONCLUSIVE" && fail_on_inconclusive),
	)
}

fn repository() -> PathBuf {
	Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// A fake agent advertising `capabilities`. It checks the options it receives and answers with
/// the exit code the engine would give `verdict` under them. `started` records a `POST /runs`.
async fn agent(
	capabilities: Value,
	policy: (bool, bool),
	verdict: &'static str,
	started: Arc<AtomicBool>,
) -> (String, tokio::task::JoinHandle<()>) {
	let code = expected_code(policy, verdict);
	let app = Router::new()
		.route("/plugins", get(move || async move { Json(capabilities) }))
		.route(
			"/runs",
			post(move |Json(request): Json<Value>| async move {
				assert_eq!(request["options"]["failOnVerdict"], policy.0);
				assert_eq!(request["options"]["failOnInconclusive"], policy.1);
				started.store(true, Ordering::SeqCst);
				Json(json!({"id":"test", "state":"running"}))
			}),
		)
		.route(
			"/runs/test",
			get(move || async move {
				let state = if code == 0 { "completed" } else { "completed_with_failures" };
				Json(json!({"id":"test", "state":state, "outcome":{
					"artifact_dir":"/agent/run-test", "exit_code":code, "state":state, "error":null
				}}))
			}),
		);
	let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
	let endpoint = format!("http://{}", listener.local_addr().unwrap());
	let server = tokio::spawn(async move {
		axum::serve(listener, app).await.unwrap();
	});
	(endpoint, server)
}

async fn run_remote(
	endpoint: &str,
	(fail_on_verdict, fail_on_inconclusive): (bool, bool),
) -> std::process::Output {
	let mut cli = tokio::process::Command::new(env!("CARGO_BIN_EXE_polkameter"));
	cli.args([
		"run",
		"examples/plugin-workflow.polkameter.xml",
		"--remote",
		endpoint,
		"--remote-token-env",
		"POLKAMETER_TEST_AGENT_TOKEN",
		"--format",
		"json",
	])
	.current_dir(repository())
	.env("POLKAMETER_TEST_AGENT_TOKEN", "test-token");
	if fail_on_verdict {
		cli.arg("--fail-on-verdict");
	}
	if fail_on_inconclusive {
		cli.arg("--fail-on-inconclusive");
	}
	cli.output().await.unwrap()
}

#[tokio::test]
async fn run_flags_are_sent_to_the_agent_and_return_its_exit_code() {
	let capabilities = json!({"runOptions":{"failOnVerdict":true,"failOnInconclusive":true}});
	for policy in POLICIES {
		for verdict in VERDICTS {
			let started = Arc::new(AtomicBool::new(false));
			let (endpoint, server) =
				agent(capabilities.clone(), policy, verdict, started.clone()).await;
			let output = run_remote(&endpoint, policy).await;
			let code = expected_code(policy, verdict);
			assert_eq!(
				output.status.code(),
				Some(code),
				"{policy:?} {verdict}: {}",
				String::from_utf8_lossy(&output.stderr)
			);
			assert!(started.load(Ordering::SeqCst));
			let event: Value = serde_json::from_slice(&output.stdout).unwrap();
			assert_eq!(event["event"], "artifact-written");
			assert_eq!(event["outcome"]["exit_code"], code);
			server.abort();
		}
	}
}

#[tokio::test]
async fn fail_on_inconclusive_refuses_an_agent_without_the_option() {
	// An agent from before --fail-on-inconclusive advertises only failOnVerdict, and would
	// silently ignore the new option.
	let older = json!({"runOptions":{"failOnVerdict":true}});
	for policy in [(false, true), (true, true)] {
		let started = Arc::new(AtomicBool::new(false));
		let (endpoint, server) =
			agent(older.clone(), policy, "INCONCLUSIVE", started.clone()).await;
		let output = run_remote(&endpoint, policy).await;
		let stderr = String::from_utf8_lossy(&output.stderr);
		assert_eq!(output.status.code(), Some(1), "{stderr}");
		assert!(
			stderr
				.contains("remote agent does not support --fail-on-inconclusive; update the agent"),
			"{stderr}"
		);
		assert!(!started.load(Ordering::SeqCst), "the run started anyway");
		server.abort();
	}
	// --fail-on-verdict alone still works with that agent.
	let started = Arc::new(AtomicBool::new(false));
	let (endpoint, server) = agent(older, (true, false), "FAIL", started.clone()).await;
	assert_eq!(run_remote(&endpoint, (true, false)).await.status.code(), Some(1));
	assert!(started.load(Ordering::SeqCst));
	server.abort();
}

struct Work(PathBuf);
impl Drop for Work {
	fn drop(&mut self) {
		let _ = fs::remove_dir_all(&self.0);
	}
}

/// A run without `<monitors>`, rebuilt by `polkameter report`, is not INCONCLUSIVE: the checks
/// of nodes it does not monitor are not applicable and optional.
#[test]
fn a_monitorless_run_is_not_inconclusive() {
	let work = Work(std::env::temp_dir().join(format!(
		"polkameter-monitorless-{}-{}",
		std::process::id(),
		std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
	)));
	fs::create_dir_all(&work.0).unwrap();
	// The recorded run's own files, less what only monitors write (node scrapes in scrapes.jsonl,
	// the relay chain in chain.jsonl) and the plugin's verdicts: what a run of the same load
	// without <monitors> leaves behind.
	let fixture = repository().join("crates/cli/tests/fixtures/smoke-run");
	for name in
		["events.jsonl", "execution.json", "load.jsonl", "steps.jsonl", "transactions.jsonl"]
	{
		fs::copy(fixture.join(name), work.0.join(name)).unwrap();
	}
	let mut summary: Value =
		serde_json::from_slice(&fs::read(fixture.join("summary.json")).unwrap()).unwrap();
	// Without <monitors>, the run records no parachain and no relay chain, and the latency
	// breaking point of the recorded run (a separate FAIL) is left out.
	summary["network"]["paraId"] = Value::Null;
	summary["breakingPoint"] = Value::Null;
	summary["mode"] = json!("stress");
	for key in ["checks", "verdict"] {
		summary.as_object_mut().unwrap().remove(key);
	}
	fs::write(work.0.join("summary.json"), summary.to_string()).unwrap();

	let output = std::process::Command::new(env!("CARGO_BIN_EXE_polkameter"))
		.args(["report", "--format", "json"])
		.arg(&work.0)
		.env("POLKAMETER_PLUGIN_REGISTRY", work.0.join("plugins.json"))
		.output()
		.unwrap();
	assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
	let summary: Value =
		serde_json::from_slice(&fs::read(work.0.join("summary.json")).unwrap()).unwrap();
	let checks = summary["checks"].as_array().unwrap();
	let not_applicable: Vec<&Value> = checks
		.iter()
		.filter(|c| c["detail"].as_str().unwrap().starts_with("not applicable:"))
		.collect();
	assert_eq!(not_applicable.len(), 7, "{checks:#?}");
	for check in &not_applicable {
		assert_eq!(check["status"], "no result", "{check}");
		assert_eq!(check["optional"], true, "{check}");
	}
	let required_gaps: Vec<&Value> = checks
		.iter()
		.filter(|c| c["status"] == "no result" && c["optional"] != true)
		.collect();
	assert!(required_gaps.is_empty(), "{required_gaps:#?}");
	let verdict = summary["verdict"]["result"].as_str().unwrap();
	assert!(matches!(verdict, "PASS" | "WARN"), "verdict {verdict}: {checks:#?}");
	let junit = fs::read_to_string(work.0.join("junit.xml")).unwrap();
	assert!(!junit.contains("INCONCLUSIVE"), "{junit}");
}
