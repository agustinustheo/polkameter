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

#[test]
fn recorded_report_matches_golden_validates_and_copies() {
	let work = Work::new("fixture");
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
fn failed_run_writes_redacted_failures_and_skips_and_report_rebuilds_them() {
	let work = Work::new("failed");
	let plan = work.0.join("plan.xml");
	fs::write(&plan, r#"<polkameter-plan xmlns="https://polkameter.dev/schema/plan" version="1" name="JUnit">
<credentials><credential id="key" profile="test"/></credentials>
<workflow id="workers" users="2" iterations="3"><step id="secret_check" use="core.assert-equal">
<input name="actual" value="a"/><input name="expected" value="b"/></step>
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
	let run = fs::read_dir(work.0.join("runs")).unwrap().next().unwrap().unwrap().path();
	let xml = fs::read_to_string(run.join("junit.xml")).unwrap();
	assert_eq!(xml, fs::read_to_string(copy).unwrap());
	assert!(!xml.contains("secret_check"));
	assert!(xml.contains("[redacted]"));
	assert!(xml.contains("<failure message="));
	assert!(xml.contains("<skipped>"));
	validate(&run.join("junit.xml"));
	fs::remove_file(run.join("junit.xml")).unwrap();
	assert!(work.cli().arg("report").arg(&run).output().unwrap().status.success());
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
	assert!(xml.len() < 40000);
	validate(&work.0.join("junit.xml"));
}
