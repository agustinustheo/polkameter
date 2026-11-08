use polkameter::engine::{
	execute::{self, EventSink, Outcome, RunOptions, RunState, Stop},
	plan::Plan,
	plugins::Registry,
};
use serde_json::Value;
use std::{
	path::{Path, PathBuf},
	sync::Arc,
};

#[path = "../../../crates/cli/tests/support/executable.rs"]
mod executable;

fn fixture() -> String {
	include_str!("../../../examples/plugin-data-flow.polkameter.xml").into()
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
	let executable = Path::new(env!("CARGO_BIN_EXE_polkameter-example-plugin"));
	let (manifest, installation) = Registry::describe(executable).await.unwrap();
	let mut registry = Registry::default();
	registry.plugins.insert(manifest.id, installation);
	registry
}
/// Runs `plan` to its outcome, sending its events to `sink`.
async fn run_plan(plan: Plan, registry: Registry, root: &Path, sink: EventSink) -> Outcome {
	execute::run_stoppable(plan, registry, root, Stop::default(), sink, RunOptions::default())
		.await
		.unwrap()
}
async fn run(xml: &str, registry: Registry, root: &Path) -> Outcome {
	run_plan(Plan::parse(xml).unwrap(), registry, root, Arc::new(|_| {})).await
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
async fn independent_plugin_feeds_builtin_and_plugin_steps() {
	let root = scratch("data-flow");
	let outcome = run(&fixture(), registry().await, &root).await;
	assert_eq!(outcome.exit_code, 0, "{:?}", outcome.error);
	let events = read_events(&outcome.artifact_dir);
	assert_eq!(finished(&events, "seed").count(), 1);
	assert_eq!(finished(&events, "verified").filter(|e| e["success"] == true).count(), 1);
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
async fn plugin_failures_fail_the_step_and_teardown_still_runs() {
	let registry = registry().await;
	// (failing setup step, plugin restarts before teardown). An operation error keeps the
	// plugin; a crash or a timeout kills it, and teardown starts it again.
	for (step, restarts) in [
		(r#"<step id="failing" use="example.fail"/>"#, 0),
		(r#"<step id="failing" use="example.crash"/>"#, 1),
		(r#"<step id="failing" use="example.invalid-output"/>"#, 0),
		(
			r#"<step id="failing" use="example.wait" timeout-ms="20"><input name="milliseconds" value="5000"/></step>"#,
			1,
		),
	] {
		let root = scratch("failure");
		let xml = plugin_plan(
			"Failure",
			&format!(
				r#"<setup>{step}</setup>
    <teardown><step id="cleanup" use="example.double"><input name="value" value="21"/></step></teardown>"#
			),
		);
		let outcome = run(&xml, registry.clone(), &root).await;
		assert_eq!((outcome.exit_code, outcome.state), (1, RunState::Failed), "{step}");
		assert!(outcome.artifact_dir.join("samples.jtl").is_file());
		let events = read_events(&outcome.artifact_dir);
		assert!(finished(&events, "failing").any(|e| e["success"] == false), "{step}");
		assert!(finished(&events, "cleanup").any(|e| e["success"] == true), "{step}");
		let restarted = events.iter().filter(|e| e["event"] == "plugin-restarted").count();
		assert_eq!(restarted, restarts, "{step}");
	}
}

#[tokio::test]
async fn teardown_runs_every_step_under_its_own_timeout_and_reports_every_error() {
	let root = scratch("teardown");
	let xml = plugin_plan(
		"Teardown",
		r#"<setup><step id="ok" use="example.double"><input name="value" value="1"/></step></setup>
    <teardown>
      <step id="refused" use="example.fail-with-message"><input name="message" value="cleanup refused"/></step>
      <step id="slow" use="example.wait" timeout-ms="50"><input name="milliseconds" value="60000"/></step>
      <step id="cleanup" use="example.double"><input name="value" value="21"/></step>
    </teardown>"#,
	);
	let started = std::time::Instant::now();
	let outcome = run(&xml, registry().await, &root).await;
	assert!(started.elapsed() < std::time::Duration::from_secs(30));
	assert_eq!(outcome.exit_code, 1);
	assert_eq!(outcome.teardown, Some(execute::Teardown::Failed));
	let events = read_events(&outcome.artifact_dir);
	assert!(finished(&events, "refused").any(|e| e["success"] == false));
	assert!(finished(&events, "slow").any(|e| e["success"] == false));
	// The timed-out call killed the plugin, which was started again for the next step.
	assert_eq!(events.iter().filter(|e| e["event"] == "plugin-restarted").count(), 1);
	assert!(finished(&events, "cleanup").any(|e| e["success"] == true));
	let failed = events.iter().find(|e| e["event"] == "cleanup-failed").unwrap();
	assert_eq!(failed["errors"].as_array().unwrap().len(), 2);
}

/// Every file under `directory`, recursively.
fn files(directory: &Path) -> Vec<PathBuf> {
	let mut out = Vec::new();
	for entry in std::fs::read_dir(directory).unwrap() {
		let path = entry.unwrap().path();
		if path.is_dir() {
			out.extend(files(&path));
		} else {
			out.push(path);
		}
	}
	out
}
#[tokio::test]
async fn credentials_and_endpoint_keys_reach_no_file_and_no_event() {
	let mut registry = registry().await;
	let variable = format!("POLKAMETER_TEST_STREAMED_SECRET_{}", std::process::id());
	// Quotes, a backslash and non-ASCII text: JSON writers escape all of them.
	let secret = "streamed\"cred\\entiäl-0123456789";
	// Unique test-only environment key; no other test reads or writes it.
	unsafe {
		std::env::set_var(&variable, secret);
	}
	registry.credentials.insert("test".into(), variable.clone());
	let short = format!("POLKAMETER_TEST_SHORT_SECRET_{}", std::process::id());
	unsafe {
		std::env::set_var(&short, "//Alic");
	}
	registry.credentials.insert("short".into(), short.clone());
	let endpoint = "wss://alice:hunter2hunter2@127.0.0.1:1/ws?apikey=APIKEY0123456789&v=1";
	let root = scratch("streamed-redaction");
	// The plan name holds the credential too: no load test, so summary.md has it in its title.
	let xml = plugin_plan(
		&format!("Plan {}", secret.replace('"', "&quot;")),
		&format!(
			r#"<credentials><credential id="secret" profile="test"/><credential id="dev" profile="short"/></credentials>
    <targets><target id="chain" endpoint="{}"/></targets>
    <setup>
      <step id="credential" use="example.log-in-pieces"><input name="message" ref="credentials.secret"/></step>
      <step id="endpoint" use="example.log-in-pieces"><input name="message" ref="targets.chain"/></step>
      <step id="pause" use="example.wait"><input name="milliseconds" value="3000"/></step>
      <step id="echo" use="core.echo"><input name="value" ref="credentials.secret"/></step>
      <step id="short" use="core.echo"><input name="value" ref="credentials.dev"/></step>
      <step id="compare" use="core.assert-equal"><input name="actual" ref="targets.chain"/><input name="expected" value="other"/></step>
    </setup>
    <teardown><step id="failure" use="example.fail-with-message"><input name="message" ref="credentials.secret"/></step></teardown>"#,
			endpoint.replace('&', "&amp;")
		),
	);
	let leaks = |text: &str| -> Vec<&str> {
		let escaped = serde_json::to_string(secret).unwrap();
		[secret, &escaped[1..escaped.len() - 1], "entiäl", "hunter2", "APIKEY"]
			.into_iter()
			.filter(|needle| text.contains(needle))
			.map(|needle| if needle == secret { "the credential" } else { "a secret part" })
			.collect()
	};
	// A failing preflight step echoes the credential; its error must not.
	let preflight = plugin_plan(
		"Preflight",
		r#"<credentials><credential id="secret" profile="test"/></credentials>
    <preflight><step id="failure" use="example.fail-with-message"><input name="message" ref="credentials.secret"/></step></preflight>"#,
	);
	let error = execute::preflight(&Plan::parse(&preflight).unwrap(), &registry)
		.await
		.unwrap_err()
		.to_string();
	assert!(leaks(&error).is_empty(), "{error}");
	let events = Arc::new(std::sync::Mutex::new(Vec::new()));
	let sink_events = events.clone();
	let paused = Arc::new(tokio::sync::Notify::new());
	let signal = paused.clone();
	let sink = Arc::new(move |event: Value| {
		if event["event"] == "step-started" && event["step"] == "pause" {
			signal.notify_one();
		}
		sink_events.lock().unwrap().push(event.to_string());
	});
	let output = root.0.clone();
	let plan = Plan::parse(&xml).unwrap();
	let run = tokio::spawn(async move { run_plan(plan, registry, &output, sink).await });
	// While the plugin waits, before any end-of-run pass: the streamed log is already redacted.
	paused.notified().await;
	let run_dir = std::fs::read_dir(&root.0).unwrap().next().unwrap().unwrap().path();
	let log = std::fs::read_to_string(run_dir.join("plugins/example/stderr.log")).unwrap();
	assert_eq!(log.matches("before [redacted] after\n").count(), 1, "{log}");
	assert!(
		log.contains(
			"before wss://[redacted]@127.0.0.1:1/ws?apikey=[redacted]&v=[redacted] after\n"
		),
		"{log}"
	);
	assert!(leaks(&log).is_empty(), "{log}");
	let outcome = run.await.unwrap();
	assert_eq!(outcome.exit_code, 1);
	let error = outcome.error.unwrap();
	assert!(error.contains("[redacted]") && leaks(&error).is_empty(), "{error}");
	let files = files(&outcome.artifact_dir);
	for name in
		["plan.json", "events.jsonl", "execution.json", "junit.xml", "samples.jtl", "summary.md"]
	{
		assert!(files.contains(&outcome.artifact_dir.join(name)), "{name} is missing");
	}
	for file in files {
		let text = String::from_utf8_lossy(&std::fs::read(&file).unwrap()).into_owned();
		assert!(leaks(&text).is_empty(), "{} leaks {:?}", file.display(), leaks(&text));
	}
	let plan = std::fs::read_to_string(outcome.artifact_dir.join("plan.json")).unwrap();
	assert!(plan.contains("wss://[redacted]@127.0.0.1:1/ws?apikey=[redacted]&v=[redacted]"));
	for event in events.lock().unwrap().iter() {
		assert!(leaks(event).is_empty(), "{event}");
	}
	// Only the short credential warns.
	let warnings: Vec<_> = read_events(&outcome.artifact_dir)
		.into_iter()
		.filter(|e| e["event"] == "warning")
		.map(|e| e["message"].to_string())
		.collect();
	assert_eq!(warnings.len(), 1, "{warnings:?}");
	assert!(warnings[0].contains("credentials.dev"), "{warnings:?}");
	unsafe {
		std::env::remove_var(variable);
		std::env::remove_var(short);
	}
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
	executable::copy_executable(&installed.executable, &copy);
	installed.executable = copy.clone();
	let tamper = copy.clone();
	let sink = Arc::new(move |event: Value| {
		if event["event"] == "phase" && event["phase"] == "teardown" {
			std::fs::write(&tamper, b"#!/bin/sh\nexit 1\n").unwrap();
		}
	});
	let outcome = run_plan(plan, registry, &root, sink).await;
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
