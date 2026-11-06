use polkameter::engine::{
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
			"plugin example version mismatch: set version=\"0.1.0\" in the plan's <plugin>, or \
			 install the build the plan expects with `polkameter plugin install \
			 <path-to-executable>`. The plan expects version 9.9.9, but version 0.1.0 is installed",
		),
		(
			&registry,
			fixture().replace("example.double", "example.unknown"),
			"(example.unknown): unknown operation example.unknown: the installed plugin example \
			 offers",
		),
		(&tampered, fixture(), "plugin example checksum mismatch:"),
		(&tampered, fixture(), "(BLAKE2 expected wrong, found "),
		(&tampered, fixture(), "run `polkameter plugin install "),
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
	// Before setup, so the run exits as preflight does.
	assert_eq!(outcome.exit_code, 3);
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
#[tokio::test]
async fn an_operation_error_keeps_the_plugin_for_later_steps_and_teardown() {
	let root = scratch("operation-error");
	let xml = plugin_plan(
		"Operation error",
		r#"<setup><step id="failing" use="example.fail"/></setup>
    <teardown>
      <step id="first" use="example.fail-with-message"><input name="message" value="cleanup refused"/></step>
      <step id="second" use="example.double"><input name="value" value="21"/></step>
    </teardown>"#,
	);
	let outcome = run(&xml, registry().await, &root).await;
	assert_eq!((outcome.exit_code, outcome.state), (1, RunState::Failed));
	let error = outcome.error.unwrap();
	assert!(error.starts_with("plugin operation failed: intentional plugin failure"), "{error}");
	assert!(error.contains("teardown failed: plugin operation failed: cleanup refused"), "{error}");
	let events = read_events(&outcome.artifact_dir);
	// The same process answered every call: nothing was killed or restarted.
	assert!(!events.iter().any(|e| e["event"] == "plugin-restarted"));
	assert!(finished(&events, "first").any(|e| e["success"] == false));
	assert!(finished(&events, "second").any(|e| e["success"] == true));
}
#[tokio::test]
async fn teardown_restarts_a_plugin_that_crashed() {
	let root = scratch("restart");
	let xml = plugin_plan(
		"Restart",
		r#"<setup><step id="failure" use="example.crash"/></setup>
    <teardown><step id="cleanup" use="example.double"><input name="value" value="21"/></step></teardown>"#,
	);
	let outcome = run(&xml, registry().await, &root).await;
	assert_eq!(outcome.exit_code, 1);
	assert_eq!(outcome.error.as_deref(), Some("plugin exited before responding"));
	let events = read_events(&outcome.artifact_dir);
	assert_eq!(events.iter().filter(|e| e["event"] == "plugin-restarted").count(), 1, "{events:?}");
	assert!(finished(&events, "cleanup").any(|e| e["success"] == true));
}
#[tokio::test]
async fn teardown_does_not_restart_a_plugin_whose_file_changed() {
	let root = scratch("restart-changed");
	let xml = plugin_plan(
		"Restart refused",
		r#"<setup><step id="failure" use="example.crash"/></setup>
    <teardown><step id="cleanup" use="example.double"><input name="value" value="21"/></step></teardown>"#,
	);
	let mut registry = registry().await;
	// Plugins::start checks the checksum, so change the record only after the run started.
	let plan = Plan::parse(&xml).unwrap();
	let installed = registry.plugins.get_mut("example").unwrap();
	let copy = root.join("copied-plugin");
	std::fs::copy(&installed.executable, &copy).unwrap();
	installed.executable = copy.clone();
	let tamper = copy.clone();
	let sink = Arc::new(move |event: Value| {
		if event["event"] == "phase" && event["phase"] == "teardown" {
			std::fs::write(&tamper, b"#!/bin/sh\nexit 1\n").unwrap();
		}
	});
	let outcome = execute::run(plan, registry, &root, CancellationToken::new(), sink)
		.await
		.unwrap();
	assert_eq!(outcome.exit_code, 1);
	let events = read_events(&outcome.artifact_dir);
	assert!(events.iter().any(|e| {
		e["event"] == "warning"
			&& e["message"]
				.as_str()
				.is_some_and(|m| m.contains("plugin example checksum mismatch"))
	}));
	assert!(finished(&events, "cleanup").any(|e| e["success"] == false));
}
#[tokio::test]
async fn teardown_steps_run_under_their_own_timeout() {
	let root = scratch("teardown-timeout");
	let xml = plugin_plan(
		"Teardown deadline",
		r#"<setup><step id="ok" use="example.double"><input name="value" value="1"/></step></setup>
    <teardown>
      <step id="slow" use="example.wait" timeout-ms="50"><input name="milliseconds" value="60000"/></step>
      <step id="cleanup" use="example.double"><input name="value" value="21"/></step>
    </teardown>"#,
	);
	let started = std::time::Instant::now();
	let outcome = run(&xml, registry().await, &root).await;
	assert!(started.elapsed() < std::time::Duration::from_secs(30));
	assert_eq!(outcome.exit_code, 1);
	assert_eq!(outcome.error.as_deref(), Some("plugin deadline exceeded"));
	let events = read_events(&outcome.artifact_dir);
	assert!(finished(&events, "slow").any(|e| e["success"] == false));
	// The timed-out call killed the plugin, which was started again for the next step.
	assert_eq!(events.iter().filter(|e| e["event"] == "plugin-restarted").count(), 1);
	assert!(finished(&events, "cleanup").any(|e| e["success"] == true));
}

/// A wrapper around the example plugin, for tests that need it to misbehave.
#[cfg(unix)]
fn wrapper(directory: &Path, body: &str) -> PathBuf {
	use std::os::unix::fs::PermissionsExt;
	let script = directory.join("wrapped-plugin");
	std::fs::write(
		&script,
		format!("#!/bin/sh\n{body}\nexec '{}'\n", env!("CARGO_BIN_EXE_polkameter-example-plugin")),
	)
	.unwrap();
	std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
	script
}

/// Polls until `ready` holds, failing after a minute.
#[cfg(unix)]
fn wait_until(what: &str, mut ready: impl FnMut() -> bool) {
	let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
	while !ready() {
		assert!(std::time::Instant::now() < deadline, "timed out waiting for {what}");
		std::thread::sleep(std::time::Duration::from_millis(10));
	}
}

#[cfg(unix)]
#[tokio::test]
async fn teardown_restarts_an_idle_plugin_that_something_else_killed() {
	let root = scratch("idle-killed");
	let xml = plugin_plan(
		"Idle plugin killed",
		r#"<setup><step id="first" use="example.wait"><input name="milliseconds" value="0"/></step></setup>
    <teardown><step id="cleanup" use="example.double"><input name="value" value="21"/></step></teardown>"#,
	);
	let marker_root = root.0.clone();
	let sink = Arc::new(move |event: Value| {
		if event["event"] != "step-finished" || event["step"] != "first" {
			return;
		}
		// The plugin answered and is idle: kill it from outside, as a stray signal would.
		let marker = std::fs::read_dir(&marker_root)
			.unwrap()
			.map(|entry| entry.unwrap().path().join("plugins/example/wait.pid"))
			.find(|path| path.is_file())
			.unwrap();
		let pid = std::fs::read_to_string(marker).unwrap();
		assert!(
			std::process::Command::new("kill")
				.args(["-KILL", pid.trim()])
				.status()
				.unwrap()
				.success()
		);
		// The host has not reaped it yet, so it stays a zombie once it is dead.
		wait_until("the plugin to die", || {
			let output = std::process::Command::new("ps")
				.args(["-o", "stat=", "-p", pid.trim()])
				.output()
				.unwrap();
			String::from_utf8_lossy(&output.stdout).trim_start().starts_with('Z')
		});
	});
	let outcome = execute::run(
		Plan::parse(&xml).unwrap(),
		registry().await,
		&root,
		CancellationToken::new(),
		sink,
	)
	.await
	.unwrap();
	assert_eq!(outcome.exit_code, 0, "{:?}", outcome.error);
	let events = read_events(&outcome.artifact_dir);
	assert_eq!(events.iter().filter(|e| e["event"] == "plugin-restarted").count(), 1);
	assert!(finished(&events, "cleanup").any(|e| e["success"] == true));
}

#[cfg(unix)]
#[tokio::test]
async fn admission_leaves_the_status_free_and_ends_when_the_host_closes() {
	use polkameter::plugin_application::{self as application, State};
	let root = scratch("admission");
	let slow = root.join("slow");
	let entered = root.join("entered");
	let script = wrapper(
		&root,
		&format!(
			"if [ -e '{}' ]; then touch '{}'; sleep 30; fi",
			slow.display(),
			entered.display()
		),
	);
	let mut registry = Registry::default();
	registry.install(&script).await.unwrap();
	// From now on, starting the plugin (to inspect the plan) takes 30 s.
	std::fs::write(&slow, "").unwrap();
	let xml = plugin_plan(
		"Admission",
		r#"<setup><step id="d" use="example.double"><input name="value" value="1"/></step></setup>"#,
	);
	let state = Arc::new(State::default());
	let start = |id: &str| {
		application::start_plan(
			Plan::parse(&xml).unwrap(),
			registry.clone(),
			root.join("runs").to_string_lossy().into(),
			state.clone(),
			Arc::new(|_| {}),
			Default::default(),
			Some(id.into()),
		)
	};
	let admitting = tokio::spawn(start("c-first"));
	let marker = entered.clone();
	tokio::task::spawn_blocking(move || wait_until("the inspection", || marker.exists()))
		.await
		.unwrap();
	// The plan is being inspected, and the status is still readable at once.
	let started = std::time::Instant::now();
	assert!(application::status(&state, "unknown").await.is_err());
	assert!(started.elapsed() < std::time::Duration::from_secs(5));
	// Another run is refused; the same run is asked to retry.
	let other = start("c-second").await.unwrap_err();
	assert_eq!((other.message.as_str(), other.retry), ("a plugin plan is already running", false));
	let again = start("c-first").await.unwrap_err();
	assert!(again.retry, "{again:?}");
	// Shutting down ends the admission without waiting for the slow plugin.
	state.closing.cancel();
	let error = admitting.await.unwrap().unwrap_err();
	assert_eq!((error.message.as_str(), error.retry), ("the agent is shutting down", true));
	assert!(started.elapsed() < std::time::Duration::from_secs(20));
	assert!(state.admitting.lock().unwrap().is_none());
}

#[tokio::test]
async fn every_teardown_error_is_reported() {
	let root = scratch("teardown-errors");
	let xml = plugin_plan(
		"Teardown errors",
		r#"<teardown>
      <step id="first" use="example.fail-with-message"><input name="message" value="first cleanup failed"/></step>
      <step id="second" use="example.fail-with-message"><input name="message" value="second cleanup failed"/></step>
    </teardown>"#,
	);
	let outcome = run(&xml, registry().await, &root).await;
	let expected = "plugin operation failed: first cleanup failed; plugin operation failed: second cleanup failed";
	assert_eq!(outcome.error.as_deref(), Some(expected));
	assert_eq!(outcome.teardown, Some(execute::Teardown::Failed));
	let events = read_events(&outcome.artifact_dir);
	let failed = events.iter().find(|e| e["event"] == "cleanup-failed").unwrap();
	assert_eq!(failed["error"], expected);
	assert_eq!(failed["errors"].as_array().unwrap().len(), 2);
}
