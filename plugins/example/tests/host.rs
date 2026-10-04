use polkameter_engine::{execute, plan::Plan, plugins::Registry};
use std::{path::PathBuf, sync::Arc};
use tokio_util::sync::CancellationToken;
fn fixture() -> String {
	include_str!("../../../examples/plugin-workflow.polkameter.xml").into()
}
fn directory(name: &str) -> PathBuf {
	let root = std::env::temp_dir().join(format!(
		"polkameter-{name}-{}-{}",
		std::process::id(),
		std::time::SystemTime::now()
			.duration_since(std::time::UNIX_EPOCH)
			.unwrap()
			.as_nanos()
	));
	std::fs::create_dir_all(&root).unwrap();
	root
}
async fn registry() -> Registry {
	let mut registry = Registry::default();
	registry
		.install(std::path::Path::new(env!("CARGO_BIN_EXE_polkameter-example-plugin")))
		.await
		.unwrap();
	registry
}
#[tokio::test]
async fn independent_plugin_feeds_builtin_and_plugin_steps_with_scoped_iterations() {
	let root = directory("workflow");
	let plan = Plan::parse(&fixture()).unwrap();
	let outcome =
		execute::run(plan, registry().await, &root, CancellationToken::new(), Arc::new(|_| {}))
			.await
			.unwrap();
	assert_eq!(outcome.exit_code, 0, "{:?}", outcome.error);
	let events = std::fs::read_to_string(outcome.artifact_dir.join("events.jsonl")).unwrap();
	let events = events
		.lines()
		.map(|s| serde_json::from_str::<serde_json::Value>(s).unwrap())
		.collect::<Vec<_>>();
	assert_eq!(
		events
			.iter()
			.filter(|e| e["event"] == "step-finished" && e["step"] == "seed")
			.count(),
		1
	);
	assert_eq!(
		events
			.iter()
			.filter(|e| e["event"] == "step-finished"
				&& e["step"] == "verified"
				&& e["success"] == true)
			.count(),
		6
	);
	std::fs::remove_dir_all(root).unwrap();
}
#[tokio::test]
async fn incompatible_version_and_unknown_operation_fail_before_setup() {
	let registry = registry().await;
	let plan = Plan::parse(&fixture().replace("version=\"0.1.0\"", "version=\"9.9.9\"")).unwrap();
	assert!(
		execute::inspect(&plan, &registry)
			.await
			.unwrap_err()
			.to_string()
			.contains("version/checksum")
	);
	let plan = Plan::parse(&fixture().replace("example.double", "example.unknown")).unwrap();
	assert!(
		execute::inspect(&plan, &registry)
			.await
			.unwrap_err()
			.to_string()
			.contains("unknown operation")
	);
}
#[tokio::test]
async fn plugin_timeout_fails_run_and_teardown_still_runs() {
	let root = directory("timeout");
	let xml = r#"<polkameter-plan xmlns="https://polkameter.dev/schema/plan" version="1" name="Deadline">
    <plugins><plugin id="example" version="0.1.0" protocol="1"/></plugins>
    <setup><step id="slow" use="example.wait" timeout-ms="20"><input name="milliseconds" value="5000"/></step></setup>
    <teardown><step id="cleanup" use="core.echo"><input name="value" value="done"/></step></teardown>
    </polkameter-plan>"#;
	let outcome = execute::run(
		Plan::parse(xml).unwrap(),
		registry().await,
		&root,
		CancellationToken::new(),
		Arc::new(|_| {}),
	)
	.await
	.unwrap();
	assert_eq!(outcome.exit_code, 1);
	assert!(outcome.error.unwrap().contains("deadline"));
	let events = std::fs::read_to_string(outcome.artifact_dir.join("events.jsonl")).unwrap();
	assert!(events.lines().any(|s| {
		let v: serde_json::Value = serde_json::from_str(s).unwrap();
		v["step"] == "cleanup" && v["event"] == "step-finished" && v["success"] == true
	}));
	std::fs::remove_dir_all(root).unwrap();
}
#[tokio::test]
async fn cancellation_stops_a_waiting_plugin() {
	let root = directory("cancel");
	let xml = r#"<polkameter-plan xmlns="https://polkameter.dev/schema/plan" version="1" name="Cancellation">
    <plugins><plugin id="example" version="0.1.0" protocol="1"/></plugins>
    <setup><step id="slow" use="example.wait" timeout-ms="65000"><input name="milliseconds" value="60000"/></step></setup>
    <teardown><step id="cleanup" use="core.echo"><input name="value" value="done"/></step></teardown>
    </polkameter-plan>"#;
	let cancel = CancellationToken::new();
	let started = Arc::new(tokio::sync::Notify::new());
	let signal = started.clone();
	let sink = Arc::new(move |event: serde_json::Value| {
		if event["event"] == "step-started" && event["step"] == "slow" {
			signal.notify_one();
		}
	});
	let cancel_wait = async {
		started.notified().await;
		// The start event arms cancellation; the public marker ensures the plugin
		// has entered its slow operation before we interrupt it.
		let pid = tokio::time::timeout(std::time::Duration::from_secs(5), async {
			loop {
				for entry in std::fs::read_dir(&root).unwrap() {
					let marker = entry.unwrap().path().join("plugins/example/wait.pid");
					if let Ok(pid) = std::fs::read_to_string(marker)
						&& !pid.is_empty()
					{
						return pid;
					}
				}
				tokio::time::sleep(std::time::Duration::from_millis(5)).await;
			}
		})
		.await;
		cancel.cancel();
		pid.expect("plugin did not write wait.pid")
	};
	let registry = registry().await;
	let (outcome, pid) = tokio::join!(
		execute::run(Plan::parse(xml).unwrap(), registry, &root, cancel.clone(), sink),
		cancel_wait
	);
	let outcome = outcome.unwrap();
	assert_eq!(outcome.exit_code, 130);
	assert_eq!(outcome.state, "stopped");
	let events = std::fs::read_to_string(outcome.artifact_dir.join("events.jsonl")).unwrap();
	assert!(!events.contains("ID mismatch"));
	assert!(events.lines().any(|line| {
		let event: serde_json::Value = serde_json::from_str(line).unwrap();
		event["step"] == "cleanup" && event["event"] == "step-finished" && event["success"] == true
	}));
	#[cfg(unix)]
	assert!(
		!std::process::Command::new("kill")
			.args(["-0", pid.trim()])
			.stderr(std::process::Stdio::null())
			.status()
			.unwrap()
			.success()
	);
	#[cfg(windows)]
	{
		let output = std::process::Command::new("tasklist")
			.args(["/FI", &format!("PID eq {}", pid.trim()), "/FO", "CSV", "/NH"])
			.output()
			.unwrap();
		assert!(output.status.success());
		assert!(!String::from_utf8_lossy(&output.stdout).contains(&format!("\"{}\"", pid.trim())));
	}
	std::fs::remove_dir_all(root).unwrap();
}
#[test]
fn invalid_references_and_xml_are_rejected() {
	for xml in [
		fixture().replace("steps.seed.value", "steps.future.value"),
		fixture().replace("<setup>", "<surprise>").replace("</setup>", "</surprise>"),
		fixture().replace("id=\"copied\"", "id=\"seed\""),
	] {
		assert!(Plan::parse(&xml).is_err());
	}
}

#[tokio::test]
async fn crashes_and_invalid_outputs_are_tool_failures_with_cleanup() {
	let registry = registry().await;
	for operation in ["crash", "invalid-output"] {
		let root = directory(operation);
		let xml = format!(
			r#"<polkameter-plan xmlns="https://polkameter.dev/schema/plan" version="1" name="Failure">
        <plugins><plugin id="example" version="0.1.0" protocol="1"/></plugins>
        <setup><step id="failure" use="example.{operation}"/></setup>
        <teardown><step id="clean" use="core.echo"><input name="value" value="done"/></step></teardown></polkameter-plan>"#
		);
		let outcome = execute::run(
			Plan::parse(&xml).unwrap(),
			registry.clone(),
			&root,
			CancellationToken::new(),
			Arc::new(|_| {}),
		)
		.await
		.unwrap();
		assert_eq!(outcome.exit_code, 1);
		let events = std::fs::read_to_string(outcome.artifact_dir.join("events.jsonl")).unwrap();
		assert!(events.contains("clean"));
		assert!(outcome.artifact_dir.join("samples.jtl").is_file());
		if operation == "crash" {
			assert!(
				std::fs::read_to_string(outcome.artifact_dir.join("plugins/example/stderr.log"))
					.unwrap()
					.contains("intentional crash")
			);
		}
		std::fs::remove_dir_all(root).unwrap();
	}
}
#[tokio::test]
async fn modified_executable_pin_is_rejected() {
	let mut registry = registry().await;
	registry.plugins.get_mut("example").unwrap().blake2 = "wrong".into();
	assert!(
		execute::inspect(&Plan::parse(&fixture()).unwrap(), &registry)
			.await
			.unwrap_err()
			.to_string()
			.contains("checksum")
	);
}
#[tokio::test]
async fn escaped_credentials_are_redacted_from_errors_and_events() {
	let mut registry = registry().await;
	let variable = format!("POLKAMETER_TEST_SECRET_{}", std::process::id());
	let secret = "secret\"with\nnewlines";
	// Unique test-only environment key; no other test reads or writes it.
	unsafe {
		std::env::set_var(&variable, secret);
	}
	registry.credentials.insert("test".into(), variable.clone());
	let root = directory("redaction");
	let xml = r#"<polkameter-plan xmlns="https://polkameter.dev/schema/plan" version="1" name="Redaction">
    <plugins><plugin id="example" version="0.1.0" protocol="1"/></plugins>
    <credentials><credential id="secret" profile="test"/></credentials>
    <setup><step id="failure" use="example.fail-with-message"><input name="message" ref="credentials.secret"/></step></setup></polkameter-plan>"#;
	let outcome = execute::run(
		Plan::parse(xml).unwrap(),
		registry,
		&root,
		CancellationToken::new(),
		Arc::new(|_| {}),
	)
	.await
	.unwrap();
	assert_eq!(outcome.exit_code, 1);
	assert!(outcome.error.unwrap().contains("[redacted]"));
	for name in ["events.jsonl", "execution.json", "samples.jtl", "summary.md"] {
		assert!(
			!std::fs::read_to_string(outcome.artifact_dir.join(name))
				.unwrap()
				.contains("newlines")
		);
	}
	unsafe {
		std::env::remove_var(variable);
	}
	std::fs::remove_dir_all(root).unwrap();
}
#[tokio::test]
async fn missing_credential_persists_failed_outcome_before_setup() {
	let root = directory("missing-credential");
	let xml = fixture().replace(
		"<setup>",
		"<credentials><credential id=\"admin\" profile=\"missing\"/></credentials><setup>",
	);
	let outcome = execute::run(
		Plan::parse(&xml).unwrap(),
		registry().await,
		&root,
		CancellationToken::new(),
		Arc::new(|_| {}),
	)
	.await
	.unwrap();
	assert_eq!(outcome.exit_code, 1);
	assert!(outcome.error.as_ref().unwrap().contains("not configured"));
	let persisted: execute::Outcome = serde_json::from_slice(
		&std::fs::read(outcome.artifact_dir.join("execution.json")).unwrap(),
	)
	.unwrap();
	assert_eq!(persisted.state, "failed");
	assert!(outcome.artifact_dir.join("summary.md").is_file());
	let events = std::fs::read_to_string(outcome.artifact_dir.join("events.jsonl")).unwrap();
	assert!(!events.contains("step-started"));
	std::fs::remove_dir_all(root).unwrap();
}
