//! Exercises the CLI process and remote option/exit-code plumbing with a controlled agent.
use axum::{
	Json, Router,
	routing::{get, post},
};
use serde_json::{Value, json};
use std::sync::{
	Arc,
	atomic::{AtomicBool, Ordering},
};

#[tokio::test]
async fn run_flag_is_sent_to_the_agent_and_returns_its_exit_code() {
	for fail_on_verdict in [false, true] {
		let received = Arc::new(AtomicBool::new(false));
		let sent = received.clone();
		let app = Router::new()
			.route("/plugins", get(|| async { Json(json!({"runOptions":{"failOnVerdict":true}})) }))
			.route(
				"/runs",
				post(move |Json(request): Json<Value>| async move {
					assert_eq!(request["options"]["failOnVerdict"], fail_on_verdict);
					sent.store(true, Ordering::SeqCst);
					Json(json!({"id":"test", "state":"running"}))
				}),
			)
			.route(
				"/runs/test",
				get(move || async move {
					let state =
						if fail_on_verdict { "completed_with_failures" } else { "completed" };
					Json(json!({"id":"test", "state":state, "outcome":{
						"artifact_dir":"/agent/run-test", "exit_code":i32::from(fail_on_verdict),
						"state":state, "error":null
					}}))
				}),
			);
		let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
		let endpoint = format!("http://{}", listener.local_addr().unwrap());
		let server = tokio::spawn(async move {
			axum::serve(listener, app).await.unwrap();
		});
		let mut cli = tokio::process::Command::new(env!("CARGO_BIN_EXE_polkameter"));
		cli.args([
			"run",
			"examples/plugin-workflow.polkameter.xml",
			"--remote",
			&endpoint,
			"--remote-token-env",
			"POLKAMETER_TEST_AGENT_TOKEN",
			"--format",
			"json",
		])
		.current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../.."))
		.env("POLKAMETER_TEST_AGENT_TOKEN", "test-token");
		if fail_on_verdict {
			cli.arg("--fail-on-verdict");
		}
		let output = cli.output().await.unwrap();
		assert_eq!(
			output.status.code(),
			Some(i32::from(fail_on_verdict)),
			"{}",
			String::from_utf8_lossy(&output.stderr)
		);
		assert!(received.load(Ordering::SeqCst));
		let event: Value = serde_json::from_slice(&output.stdout).unwrap();
		assert_eq!(event["event"], "artifact-written");
		assert_eq!(event["outcome"]["exit_code"], i32::from(fail_on_verdict));
		server.abort();
	}
}
