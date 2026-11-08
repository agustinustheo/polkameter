//! Process-level JUnit replay, copies, redaction and schema validation.
use std::{fs, path::Path, process::Command};

#[path = "support/smoke_run.rs"]
mod smoke_run;
#[path = "support/work.rs"]
mod work;

use serde_json::Value;
use work::Work;

impl Work {
	fn cli(&self) -> Command {
		let mut command = Command::new(env!("CARGO_BIN_EXE_polkameter"));
		command.env("POLKAMETER_PLUGIN_REGISTRY", self.0.join("plugins.json"));
		command
	}
}

fn validate(path: &Path) {
	let schema = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/schemas/junit.xsd");
	match Command::new("xmllint")
		.args(["--nonet", "--noout", "--schema"])
		.arg(schema)
		.arg(path)
		.output()
	{
		Ok(output) => {
			assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr))
		},
		Err(e) if e.kind() == std::io::ErrorKind::NotFound && std::env::var_os("CI").is_none() => {
			eprintln!("xmllint unavailable locally; skipping JUnit schema validation")
		},
		Err(e) => panic!("JUnit schema validation requires xmllint in CI: {e}"),
	}
}

#[test]
fn recorded_report_matches_golden_validates_and_copies() {
	let work = Work::new("fixture");
	smoke_run::copy_to(&work.0);
	let copy = work.0.join("reports/load.xml");
	let output = work
		.cli()
		.arg("report")
		.arg(&work.0)
		.arg("--junit")
		.arg(&copy)
		.output()
		.unwrap();
	assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
	let xml_path = work.0.join("junit.xml");
	let xml = fs::read_to_string(&xml_path).unwrap();
	assert_eq!(xml, fs::read_to_string(copy).unwrap());
	let normalized = xml.replace(&work.0.display().to_string(), "/runs/smoke-run");
	assert_eq!(normalized, include_str!("fixtures/smoke-run/junit.xml"));
	validate(&xml_path);
	// samples.jtl: JMeter CSV, workflow and setup steps stamped with their start time.
	let jtl = fs::read_to_string(work.0.join("samples.jtl")).unwrap();
	assert_eq!(jtl, include_str!("fixtures/smoke-run/samples.jtl"));
	// Rebuilding is deterministic and accepts a destination equal to the default file.
	assert!(
		work.cli()
			.arg("report")
			.arg(&work.0)
			.arg("--junit")
			.arg(&xml_path)
			.output()
			.unwrap()
			.status
			.success()
	);
	assert_eq!(xml, fs::read_to_string(xml_path).unwrap());
}

#[test]
fn report_refuses_an_unknown_summary_schema_and_rewrites_nothing() {
	let work = Work::new("schema");
	smoke_run::copy_to(&work.0);
	let path = work.0.join("summary.json");
	let mut summary: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
	for (version, shown) in [(Some(serde_json::json!(2)), "2"), (None, "missing")] {
		match &version {
			Some(v) => summary["schemaVersion"] = v.clone(),
			None => {
				summary.as_object_mut().unwrap().remove("schemaVersion");
			},
		}
		let text = serde_json::to_string_pretty(&summary).unwrap();
		fs::write(&path, &text).unwrap();
		let output = work.cli().arg("report").arg(&work.0).output().unwrap();
		assert_eq!(output.status.code(), Some(2));
		let stderr = String::from_utf8_lossy(&output.stderr);
		assert!(stderr.contains(&format!("has schemaVersion {shown}")), "{stderr}");
		assert!(stderr.contains("reads and writes only schemaVersion 1"), "{stderr}");
		assert_eq!(fs::read_to_string(&path).unwrap(), text);
		for derived in ["summary.md", "junit.xml", "samples.jtl", "plots"] {
			assert!(!work.0.join(derived).exists(), "{derived} was written");
		}
	}
}

#[test]
fn failed_run_writes_redacted_failures_and_skips_and_report_rebuilds_them() {
	let work = Work::new("failed");
	let plan = work.0.join("plan.xml");
	fs::write(&plan, r#"<polkameter-plan xmlns="https://polkameter.dev/schema/plan" version="1" name="JUnit">
<credentials><credential id="key" profile="test"/></credentials>
<workflow id="workers" users="2" iterations="3"><step id="secret_check" use="core.assert-equal">
<input name="actual" ref="credentials.key"/><input name="expected" value="b"/></step>
<step id="later" use="core.assert-equal"><input name="actual" value="a"/><input name="expected" value="a"/></step></workflow>
</polkameter-plan>"#).unwrap();
	assert!(
		work.cli()
			.args(["plugin", "credential", "test", "POLKAMETER_JUNIT_SECRET"])
			.output()
			.unwrap()
			.status
			.success()
	);
	let copy = work.0.join("reports/failure.xml");
	let output = work
		.cli()
		.arg("run")
		.arg(plan)
		.arg("--output")
		.arg(work.0.join("runs"))
		.arg("--junit")
		.arg(&copy)
		.env("POLKAMETER_JUNIT_SECRET", "secret_check")
		.output()
		.unwrap();
	assert_eq!(output.status.code(), Some(1));
	let runs: Vec<_> =
		fs::read_dir(work.0.join("runs")).unwrap().map(|r| r.unwrap().path()).collect();
	assert_eq!(runs.len(), 1);
	let run = &runs[0];
	let xml = fs::read_to_string(run.join("junit.xml")).unwrap();
	assert_eq!(xml, fs::read_to_string(copy).unwrap());
	assert!(!xml.contains("secret_check"));
	assert!(xml.contains("expected &quot;b&quot;, got &quot;[redacted]&quot;"));
	for name in ["events.jsonl", "execution.json"] {
		let content = fs::read_to_string(run.join(name)).unwrap();
		assert!(!content.contains("secret_check"));
		assert!(serde_json::Deserializer::from_str(&content).into_iter::<Value>().any(|event| {
			let event = event.unwrap();
			event["error"]
				.as_str()
				.is_some_and(|error| error.contains(r#"expected "b", got "[redacted]""#))
		}));
	}
	assert!(String::from_utf8_lossy(&output.stderr).contains(r#"expected "b", got "[redacted]""#));
	assert!(xml.contains("[redacted]"));
	assert!(xml.contains("<failure message="));
	assert!(xml.contains("<skipped>"));
	validate(&run.join("junit.xml"));
	fs::remove_file(run.join("junit.xml")).unwrap();
	assert!(work.cli().arg("report").arg(run).output().unwrap().status.success());
	let replay = fs::read_to_string(run.join("junit.xml")).unwrap();
	assert!(!replay.contains("secret_check"));
	assert_eq!(replay, xml);

	// A hostile recorded message is escaped, loses its control characters and is bounded.
	let mut execution: Value =
		serde_json::from_slice(&fs::read(run.join("execution.json")).unwrap()).unwrap();
	execution["error"] = format!("<&>\"'\n\t\0\u{1b}\u{202e}\u{ffff}{}", "界&".repeat(5000)).into();
	fs::write(run.join("execution.json"), serde_json::to_vec(&execution).unwrap()).unwrap();
	assert!(work.cli().arg("report").arg(run).output().unwrap().status.success());
	let hostile = fs::read_to_string(run.join("junit.xml")).unwrap();
	assert!(hostile.contains("&lt;&amp;&gt;&quot;&apos;") && hostile.contains('…'));
	assert!(!hostile.contains(['\0', '\u{1b}', '\u{202e}']));
	assert!(hostile.len() < xml.len() + polkameter::engine::junit::MESSAGE_CHARS * 6 * 2);
	validate(&run.join("junit.xml"));
}

#[test]
fn offline_report_redacts_available_registry_credentials_in_details() {
	let work = Work::new("offline-secret");
	let secret = "secret-in-error-detail";
	fs::write(work.0.join("plugins.json"), r#"{"credentials":{"test":"POLKAMETER_JUNIT_SECRET"}}"#)
		.unwrap();
	fs::write(work.0.join("events.jsonl"), "").unwrap();
	fs::write(work.0.join("summary.md"), "failed").unwrap();
	fs::write(
		work.0.join("execution.json"),
		serde_json::to_vec(&serde_json::json!({
			"artifact_dir":work.0,"exit_code":1,"state":"failed","error":format!("plugin detail: {secret}")
		}))
		.unwrap(),
	)
	.unwrap();
	let output = work
		.cli()
		.arg("report")
		.arg(&work.0)
		.env("POLKAMETER_JUNIT_SECRET", secret)
		.output()
		.unwrap();
	assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
	let xml = fs::read_to_string(work.0.join("junit.xml")).unwrap();
	assert!(xml.contains("plugin detail: [redacted]"));
	assert!(!xml.contains(secret));
	validate(&work.0.join("junit.xml"));
}

/// A JUnit copy that cannot be written, or fetched from the agent, is a warning: the run keeps
/// exit 0.
#[tokio::test]
async fn a_junit_copy_failure_warns_and_keeps_exit_0() {
	use axum::{
		Json, Router,
		routing::{get, post},
	};
	use serde_json::json;
	let work = Work::new("junit-copy-errors");
	let app = Router::new()
		.route(
			"/runs",
			post(|| async {
				Json(
					json!({"id":"test","state":"running","phase":null,"outcome":null,"error":null}),
				)
			}),
		)
		.route(
			"/runs/test",
			get(|| async {
				Json(json!({"id":"test", "state":"completed", "phase":null, "error":null,
					"outcome":{"artifact_dir":"/agent/run", "state":"completed", "exit_code":0,
						"error":null}}))
			}),
		)
		.route(
			"/runs/test/junit",
			get(|| async { (axum::http::StatusCode::NOT_FOUND, Json("report missing")) }),
		);
	let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
	let endpoint = format!("http://{}", listener.local_addr().unwrap());
	let server = tokio::spawn(async move {
		axum::serve(listener, app).await.unwrap();
	});
	let plan = work.0.join("plan.xml");
	fs::write(&plan, r#"<polkameter-plan xmlns="https://polkameter.dev/schema/plan" version="1" name="test"><setup><step id="ok" use="core.echo"><input name="value" value="1"/></step></setup></polkameter-plan>"#).unwrap();
	#[cfg(unix)]
	let blocker = work.0.join("blocker & 'quoted'\n\t\u{1b}\u{202e}");
	#[cfg(not(unix))]
	let blocker = work.0.join("blocker & 'quoted'");
	fs::write(&blocker, "a file, so no directory can be created under it").unwrap();
	let runs = work.0.join("runs");
	let local = &["--output", runs.to_str().unwrap()][..];
	let remote = &["--remote", &endpoint, "--remote-token-env", "POLKAMETER_JUNIT_TOKEN"][..];
	for (destination, args) in [
		(work.0.clone(), local),
		(blocker.join("report.xml"), local),
		(work.0.join("report.xml"), remote),
	] {
		let mut command = work.cli();
		command
			.arg("run")
			.arg(&plan)
			.args(args)
			.args(["--format", "json", "--junit"])
			.arg(destination)
			.env("POLKAMETER_JUNIT_TOKEN", "test-token");
		let output = tokio::task::spawn_blocking(move || command.output().unwrap()).await.unwrap();
		let stderr = String::from_utf8(output.stderr).unwrap();
		assert_eq!(output.status.code(), Some(0), "{stderr}");
		assert_eq!(stderr.lines().count(), 1, "{stderr}");
		assert!(!stderr.contains(['\t', '\u{1b}', '\u{202e}']), "{stderr}");
		assert!(!stderr.contains("&amp;") && !stderr.contains("&apos;"), "{stderr}");
		let events: Vec<Value> = String::from_utf8(output.stdout)
			.unwrap()
			.lines()
			.map(|line| serde_json::from_str(line).unwrap())
			.collect();
		assert!(events.iter().any(|e| e["event"] == "warning"));
		let artifact = events.last().unwrap();
		assert_eq!(artifact["event"], "artifact-written");
		assert_eq!(artifact["outcome"]["exit_code"], 0);
	}
	server.abort();
}
