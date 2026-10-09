use polkameter_engine::{
	execute::{self, Outcome, RunState},
	plan::Plan,
	plugins::Registry,
};
use serde_json::Value;
use std::{
	path::{Path, PathBuf},
	sync::Arc,
};
use tokio_util::sync::CancellationToken;

fn fixture() -> String {
	include_str!("../../../examples/plugin-workflow.polkameter.xml").into()
}
/// A plan that installs the example plugin; `body` holds its credentials, setup and teardown.
fn plugin_plan(name: &str, body: &str) -> String {
	format!(
		r#"<polkameter-plan xmlns="https://polkameter.dev/schema/plan" version="1" name="{name}"><plugins><plugin id="example" version="0.1.0" protocol="1"/></plugins>{body}</polkameter-plan>"#
	)
}
/// A scratch directory that is removed when the test ends, even if it fails.
struct Scratch(PathBuf);
impl std::ops::Deref for Scratch {
	type Target = Path;
	fn deref(&self) -> &Path {
		&self.0
	}
}
impl AsRef<Path> for Scratch {
	fn as_ref(&self) -> &Path {
		&self.0
	}
}
impl Drop for Scratch {
	fn drop(&mut self) {
		let _ = std::fs::remove_dir_all(&self.0);
	}
}
fn scratch(name: &str) -> Scratch {
	let path = std::env::temp_dir().join(format!(
		"polkameter-{name}-{}-{}",
		std::process::id(),
		std::time::SystemTime::now()
			.duration_since(std::time::UNIX_EPOCH)
			.unwrap()
			.as_nanos()
	));
	std::fs::create_dir_all(&path).unwrap();
	Scratch(path)
}
async fn registry() -> Registry {
	let mut registry = Registry::default();
	registry
		.install(Path::new(env!("CARGO_BIN_EXE_polkameter-example-plugin")))
		.await
		.unwrap();
	registry
}
async fn run(xml: &str, registry: Registry, root: &Path) -> Outcome {
	execute::run(
		Plan::parse(xml).unwrap(),
		registry,
		root,
		CancellationToken::new(),
		Arc::new(|_| {}),
	)
	.await
	.unwrap()
}
/// The parsed events.jsonl of a run.
fn read_events(directory: &Path) -> Vec<Value> {
	std::fs::read_to_string(directory.join("events.jsonl"))
		.unwrap()
		.lines()
		.map(|line| serde_json::from_str(line).unwrap())
		.collect()
}
/// The `step-finished` events of `step`, in order.
fn finished<'a>(events: &'a [Value], step: &'a str) -> impl Iterator<Item = &'a Value> {
	events
		.iter()
		.filter(move |e| e["event"] == "step-finished" && e["step"] == step)
}

#[tokio::test]
async fn independent_plugin_feeds_builtin_and_plugin_steps_with_scoped_iterations() {
	let root = scratch("workflow");
	let outcome = run(&fixture(), registry().await, &root).await;
	assert_eq!(outcome.exit_code, 0, "{:?}", outcome.error);
	let events = read_events(&outcome.artifact_dir);
	assert_eq!(finished(&events, "seed").count(), 1);
	assert_eq!(finished(&events, "verified").filter(|e| e["success"] == true).count(), 6);
}
#[tokio::test]
async fn incompatible_version_and_unknown_operation_fail_before_setup() {
	let registry = registry().await;
	let mut tampered = registry.clone();
	tampered.plugins.get_mut("example").unwrap().blake2 = "wrong".into();
	let cases = [
		(
			&registry,
			fixture().replace("version=\"0.1.0\"", "version=\"9.9.9\""),
			"version/checksum",
		),
		(&registry, fixture().replace("example.double", "example.unknown"), "unknown operation"),
		(&tampered, fixture(), "checksum"),
	];
	for (registry, xml, expected) in cases {
		let error = execute::inspect(&Plan::parse(&xml).unwrap(), registry).await.unwrap_err();
		assert!(error.to_string().contains(expected), "{error}");
	}
}
#[tokio::test]
async fn plugin_timeout_fails_run_and_teardown_still_runs() {
	let root = scratch("timeout");
	let xml = plugin_plan(
		"Deadline",
		r#"<setup><step id="slow" use="example.wait" timeout-ms="20"><input name="milliseconds" value="5000"/></step></setup>
    <teardown><step id="cleanup" use="core.echo"><input name="value" value="done"/></step></teardown>"#,
	);
	let outcome = run(&xml, registry().await, &root).await;
	assert_eq!(outcome.exit_code, 1);
	assert!(outcome.error.unwrap().contains("deadline"));
	let events = read_events(&outcome.artifact_dir);
	assert!(finished(&events, "cleanup").any(|e| e["success"] == true));
}
#[tokio::test]
async fn cancellation_stops_a_waiting_plugin() {
	let root = scratch("cancel");
	let xml = plugin_plan(
		"Cancellation",
		r#"<setup><step id="slow" use="example.wait" timeout-ms="65000"><input name="milliseconds" value="60000"/></step></setup>
    <teardown><step id="cleanup" use="core.echo"><input name="value" value="done"/></step></teardown>"#,
	);
	let cancel = CancellationToken::new();
	let started = Arc::new(tokio::sync::Notify::new());
	let signal = started.clone();
	let sink = Arc::new(move |event: Value| {
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
	let (outcome, pid) = tokio::join!(
		execute::run(Plan::parse(&xml).unwrap(), registry().await, &root, cancel.clone(), sink),
		cancel_wait
	);
	let outcome = outcome.unwrap();
	assert_eq!(outcome.exit_code, 130);
	assert_eq!(outcome.state, RunState::Stopped);
	let events = read_events(&outcome.artifact_dir);
	assert!(!events.iter().any(|e| e.to_string().contains("ID mismatch")));
	assert!(finished(&events, "cleanup").any(|e| e["success"] == true));
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
		let root = scratch(operation);
		let xml = plugin_plan(
			"Failure",
			&format!(
				r#"<setup><step id="failure" use="example.{operation}"/></setup>
        <teardown><step id="clean" use="core.echo"><input name="value" value="done"/></step></teardown>"#
			),
		);
		let outcome = run(&xml, registry.clone(), &root).await;
		assert_eq!(outcome.exit_code, 1);
		let events = read_events(&outcome.artifact_dir);
		assert!(finished(&events, "clean").any(|e| e["success"] == true));
		assert!(outcome.artifact_dir.join("samples.jtl").is_file());
		if operation == "crash" {
			assert!(
				std::fs::read_to_string(outcome.artifact_dir.join("plugins/example/stderr.log"))
					.unwrap()
					.contains("intentional crash")
			);
		}
	}
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
	let root = scratch("redaction");
	let xml = plugin_plan(
		"Redaction",
		r#"<credentials><credential id="secret" profile="test"/></credentials>
    <setup><step id="failure" use="example.fail-with-message"><input name="message" ref="credentials.secret"/></step></setup>"#,
	);
	let preflight_xml = plugin_plan(
		"Redaction",
		r#"<credentials><credential id="secret" profile="test"/></credentials>
    <preflight><step id="failure" use="example.fail-with-message"><input name="message" ref="credentials.secret"/></step></preflight>"#,
	);
	let error = execute::preflight(&Plan::parse(&preflight_xml).unwrap(), &registry)
		.await
		.unwrap_err()
		.to_string();
	assert!(error.contains("[redacted]") && !error.contains("newlines"), "{error}");
	let outcome = run(&xml, registry, &root).await;
	assert_eq!(outcome.exit_code, 1);
	assert!(outcome.error.unwrap().contains("[redacted]"));
	// The plugin wrote the secret to its stderr; the log keeps only the redacted text.
	let logs = std::fs::read_dir(outcome.artifact_dir.join("plugins")).unwrap();
	let mut redacted_logs = 0;
	for log in logs {
		let text = std::fs::read_to_string(log.unwrap().path().join("stderr.log")).unwrap();
		assert!(!text.contains("newlines"), "{text}");
		redacted_logs += usize::from(text.contains("[redacted]"));
	}
	assert_eq!(redacted_logs, 1, "the example plugin logged the redacted message");
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
}
#[tokio::test]
async fn missing_credential_persists_failed_outcome_before_setup() {
	let root = scratch("missing-credential");
	let xml = fixture().replace(
		"<setup>",
		"<credentials><credential id=\"admin\" profile=\"missing\"/></credentials><setup>",
	);
	let outcome = run(&xml, registry().await, &root).await;
	assert_eq!(outcome.exit_code, 1);
	assert!(outcome.error.as_ref().unwrap().contains("not configured"));
	let persisted: Outcome = serde_json::from_slice(
		&std::fs::read(outcome.artifact_dir.join("execution.json")).unwrap(),
	)
	.unwrap();
	assert_eq!(persisted.state, RunState::Failed);
	assert!(outcome.artifact_dir.join("summary.md").is_file());
	let events = read_events(&outcome.artifact_dir);
	assert!(!events.iter().any(|e| e["event"] == "step-started"));
}
