//! Process-level JUnit replay, copies, redaction and schema validation.
use std::{
	fs,
	path::{Path, PathBuf},
	process::Command,
};

struct Work(PathBuf);
impl Work {
	fn new(label: &str) -> Self {
		let path = std::env::temp_dir().join(format!(
			"polkameter-junit-{label}-{}-{}",
			std::process::id(),
			std::time::SystemTime::now()
				.duration_since(std::time::UNIX_EPOCH)
				.unwrap()
				.as_nanos()
		));
		fs::create_dir_all(&path).unwrap();
		Self(path.canonicalize().unwrap())
	}
	fn cli(&self) -> Command {
		let mut command = Command::new(env!("CARGO_BIN_EXE_polkameter"));
		command.env("POLKAMETER_PLUGIN_REGISTRY", self.0.join("plugins.json"));
		command
	}
}
impl Drop for Work {
	fn drop(&mut self) {
		let _ = fs::remove_dir_all(&self.0);
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

/// Copies the recorded smoke run's input files (not its golden outputs) into `work`.
fn copy_fixture(work: &Work) {
	let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/smoke-run");
	for entry in fs::read_dir(&fixture).unwrap() {
		let path = entry.unwrap().path();
		if path.extension().is_some_and(|e| e == "gz") {
			let mut decoder = flate2::read::GzDecoder::new(fs::File::open(&path).unwrap());
			let mut file = fs::File::create(work.0.join(path.file_stem().unwrap())).unwrap();
			std::io::copy(&mut decoder, &mut file).unwrap();
		} else if path.extension().is_some_and(|e| e == "json" || e == "jsonl") {
			fs::copy(&path, work.0.join(path.file_name().unwrap())).unwrap();
		}
	}
}

#[test]
fn recorded_report_matches_golden_validates_and_copies() {
	let work = Work::new("fixture");
	copy_fixture(&work);
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
	copy_fixture(&work);
	let path = work.0.join("summary.json");
	let mut summary: serde_json::Value =
		serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
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
		assert!(
			serde_json::Deserializer::from_str(&content)
				.into_iter::<serde_json::Value>()
				.any(|event| {
					let event = event.unwrap();
					event["error"]
						.as_str()
						.is_some_and(|error| error.contains(r#"expected "b", got "[redacted]""#))
				})
		);
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
}

#[test]
fn report_escapes_and_bounds_recorded_messages_and_validates_the_xml() {
	let work = Work::new("escaping");
	let error = format!("<&>\"'\n\t\0\u{1b}\u{202e}\u{ffff}{}", "界&".repeat(5000));
	fs::write(
		work.0.join("execution.json"),
		serde_json::to_vec(&serde_json::json!({
			"artifact_dir":work.0, "exit_code":1, "state":"failed", "error":error
		}))
		.unwrap(),
	)
	.unwrap();
	fs::write(work.0.join("events.jsonl"), "").unwrap();
	fs::write(work.0.join("summary.md"), "failed").unwrap();
	let output = work.cli().arg("report").arg(&work.0).output().unwrap();
	assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
	let xml = fs::read_to_string(work.0.join("junit.xml")).unwrap();
	assert!(xml.contains("&lt;&amp;&gt;&quot;&apos;"));
	assert!(xml.contains('…'));
	assert!(!xml.contains('\0'));
	assert!(!xml.contains('\u{1b}'));
	assert!(!xml.contains('\u{202e}'));
	assert!(xml.len() < polkameter::engine::junit::MESSAGE_CHARS * 6 * 2 + 2048);
	validate(&work.0.join("junit.xml"));
}

#[test]
fn invalid_junit_copy_destinations_warn_without_changing_success() {
	let work = Work::new("copy-errors");
	let plan = work.0.join("plan.xml");
	fs::write(&plan, r#"<polkameter-plan xmlns="https://polkameter.dev/schema/plan" version="1" name="test"><setup><step id="ok" use="core.echo"><input name="value" value="1"/></step></setup></polkameter-plan>"#).unwrap();
	#[cfg(unix)]
	let blocker_name = "blocker & 'quoted'\n\t\u{1b}\u{202e}";
	#[cfg(not(unix))]
	let blocker_name = "blocker & 'quoted'";
	let blocker = work.0.join(blocker_name);
	fs::write(&blocker, "file, cannot create a directory under it").unwrap();
	for destination in [work.0.clone(), blocker.join("report.xml")] {
		let output = work
			.cli()
			.arg("run")
			.arg(&plan)
			.arg("--output")
			.arg(work.0.join("runs"))
			.arg("--junit")
			.arg(&destination)
			.args(["--format", "json"])
			.output()
			.unwrap();
		assert!(output.status.success());
		let stderr = String::from_utf8(output.stderr).unwrap();
		assert_eq!(stderr.lines().count(), 1, "{stderr}");
		assert!(stderr.contains("Polkameter warning: JUnit report:"));
		assert!(!stderr.contains("&amp;") && !stderr.contains("&apos;"), "{stderr}");
		assert!(!stderr.contains(['\t', '\u{1b}', '\u{202e}']), "{stderr}");
		if destination != work.0 {
			#[cfg(unix)]
			assert!(stderr.contains("blocker & 'quoted'    /report.xml"), "{stderr}");
			#[cfg(not(unix))]
			assert!(stderr.contains(blocker_name), "{stderr}");
		}
		let events: Vec<serde_json::Value> = String::from_utf8(output.stdout)
			.unwrap()
			.lines()
			.map(|s| serde_json::from_str(s).unwrap())
			.collect();
		assert!(events.iter().any(|e| e["event"] == "warning"));
		let artifact = events.iter().find(|e| e["event"] == "artifact-written").unwrap();
		assert_eq!(artifact["outcome"]["exit_code"], 0);
		let run = Path::new(artifact["outcome"]["artifact_dir"].as_str().unwrap());
		assert!(
			fs::read_to_string(run.join("events.jsonl"))
				.unwrap()
				.contains("\"event\":\"warning\"")
		);
		validate(&run.join("junit.xml"));
		let replay = work
			.cli()
			.arg("report")
			.arg(run)
			.arg("--junit")
			.arg(destination)
			.output()
			.unwrap();
		assert!(replay.status.success());
		assert!(
			String::from_utf8_lossy(&replay.stderr).contains("Polkameter warning: JUnit report:")
		);
	}
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

#[tokio::test]
async fn remote_junit_fetch_and_copy_failures_preserve_success_and_cancellation() {
	use axum::{
		Json, Router,
		routing::{get, post},
	};
	use serde_json::{Value, json};
	for code in [0, 130] {
		for fetch_fails in [true, false] {
			let work = Work::new("remote-errors");
			let state = if code == 0 { "completed" } else { "stopped" };
			let status = json!({"id":"test", "state":state, "phase":null, "error":null,
                "outcome":{"artifact_dir":"/agent/run", "state":state, "exit_code":code,
                    "error":if code == 0 { Value::Null } else { json!("run cancelled") }}});
			let app = Router::new()
				.route(
					"/runs",
					post(|| async {
						Json(
							json!({"id":"test","state":"running","phase":null,"outcome":null,"error":null}),
						)
					}),
				)
				.route("/runs/test", get(move || async move { Json(status) }))
				.route(
					"/runs/test/junit",
					get(move || async move {
						if fetch_fails {
							(axum::http::StatusCode::NOT_FOUND, Json("report missing"))
						} else {
							(axum::http::StatusCode::OK, Json("<testsuites/>"))
						}
					}),
				);
			let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
			let endpoint = format!("http://{}", listener.local_addr().unwrap());
			let server = tokio::spawn(async move {
				axum::serve(listener, app).await.unwrap();
			});
			let plan = work.0.join("plan.xml");
			fs::write(&plan, r#"<polkameter-plan xmlns="https://polkameter.dev/schema/plan" version="1" name="test"><setup><step id="ok" use="core.echo"><input name="value" value="1"/></step></setup></polkameter-plan>"#).unwrap();
			let mut command = work.cli();
			command
				.arg("run")
				.arg(plan)
				.args([
					"--remote",
					&endpoint,
					"--remote-token-env",
					"POLKAMETER_JUNIT_TOKEN",
					"--format",
					"json",
				])
				.arg("--junit")
				.arg(&work.0)
				.env("POLKAMETER_JUNIT_TOKEN", "test-token");
			let output =
				tokio::task::spawn_blocking(move || command.output().unwrap()).await.unwrap();
			server.abort();
			assert_eq!(output.status.code(), Some(code));
			let events: Vec<serde_json::Value> = String::from_utf8(output.stdout)
				.unwrap()
				.lines()
				.map(|line| serde_json::from_str(line).unwrap())
				.collect();
			let warning = events.iter().find(|e| e["event"] == "warning").unwrap();
			assert!(warning["message"].as_str().unwrap().contains(if fetch_fails {
				"cannot fetch"
			} else {
				"cannot write"
			}));
			let artifact = events.last().unwrap();
			assert_eq!(artifact["event"], "artifact-written");
			assert_eq!(artifact["outcome"]["exit_code"], code);
			assert!(
				String::from_utf8_lossy(&output.stderr)
					.contains("Polkameter warning: JUnit report:")
			);
		}
	}
}

#[cfg(unix)]
#[test]
fn local_copy_failure_preserves_sigint_exit_130_and_artifact_notification() {
	use std::time::{Duration, Instant};
	let work = Work::new("cancel-copy");
	let plan = work.0.join("plan.xml");
	fs::write(&plan, r#"<polkameter-plan xmlns="https://polkameter.dev/schema/plan" version="1" name="cancel"><workflow id="workers" users="1" iterations="1000000000"><step id="work" use="core.echo"><input name="value" value="1"/></step></workflow></polkameter-plan>"#).unwrap();
	let root = work.0.join("runs");
	// The output root can exist before the child creates its run directory.
	fs::create_dir_all(&root).unwrap();
	let mut child = work
		.cli()
		.arg("run")
		.arg(plan)
		.arg("--output")
		.arg(&root)
		.arg("--junit")
		.arg(&work.0)
		.stdout(std::process::Stdio::piped())
		.stderr(std::process::Stdio::piped())
		.spawn()
		.unwrap();
	let deadline = Instant::now() + Duration::from_secs(20);
	loop {
		if let Ok(runs) = fs::read_dir(&root) {
			let runs: Vec<_> = runs.map(|r| r.unwrap().path()).collect();
			assert!(runs.len() <= 1, "unexpected run directories: {runs:?}");
			if runs.first().is_some_and(|run| {
				fs::read_to_string(run.join("events.jsonl"))
					.unwrap_or_default()
					.contains("step-started")
			}) {
				break;
			}
		}
		if Instant::now() >= deadline {
			child.kill().unwrap();
			let output = child.wait_with_output().unwrap();
			panic!("run did not start: {}", String::from_utf8_lossy(&output.stderr));
		}
		std::thread::sleep(Duration::from_millis(10));
	}
	assert!(
		Command::new("kill")
			.args(["-INT", &child.id().to_string()])
			.status()
			.unwrap()
			.success()
	);
	let output = child.wait_with_output().unwrap();
	assert_eq!(output.status.code(), Some(130));
	assert!(String::from_utf8_lossy(&output.stdout).contains("Run stopped. Artifacts:"));
	assert!(String::from_utf8_lossy(&output.stderr).contains("Polkameter warning: JUnit report:"));
	let runs: Vec<_> = fs::read_dir(root).unwrap().map(|r| r.unwrap().path()).collect();
	assert_eq!(runs.len(), 1);
	let execution: serde_json::Value =
		serde_json::from_slice(&fs::read(runs[0].join("execution.json")).unwrap()).unwrap();
	assert_eq!(execution["exit_code"], 130);
	assert_eq!(execution["state"], "stopped");
	validate(&runs[0].join("junit.xml"));
}
