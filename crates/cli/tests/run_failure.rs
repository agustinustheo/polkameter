//! A failed `polkameter run` prints its reason on standard error, keeps exit code 1 and the run
//! directory, and keeps standard output to the usual lines.

use std::{
	path::{Path, PathBuf},
	process::Command,
};

use serde_json::Value;

const FAILING_PLAN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<polkameter-plan xmlns="https://polkameter.dev/schema/plan" version="1" name="Failing" timeout-ms="30000">
  <setup>
    <step id="check" use="core.assert-equal"><input name="actual" value="1"/><input name="expected" value="2"/></step>
  </setup>
</polkameter-plan>
"#;

fn work_directory(name: &str) -> PathBuf {
	let directory = std::env::temp_dir().join(format!(
		"polkameter-run-failure-{name}-{}-{}",
		std::process::id(),
		std::time::SystemTime::now()
			.duration_since(std::time::UNIX_EPOCH)
			.unwrap()
			.as_nanos()
	));
	std::fs::create_dir_all(&directory).unwrap();
	std::fs::write(directory.join("plan.xml"), FAILING_PLAN).unwrap();
	// The CLI prints canonical paths, and the temporary directory can be a symlink (macOS).
	directory.canonicalize().unwrap()
}

fn run(directory: &Path, format: &str) -> (i32, String, String) {
	let output = Command::new(env!("CARGO_BIN_EXE_polkameter"))
		.arg("run")
		.arg(directory.join("plan.xml"))
		.arg("--output")
		.arg(directory.join("runs"))
		.args(["--format", format])
		.env("POLKAMETER_PLUGIN_REGISTRY", directory.join("plugins.json"))
		.output()
		.unwrap();
	(
		output.status.code().unwrap(),
		String::from_utf8(output.stdout).unwrap(),
		String::from_utf8(output.stderr).unwrap(),
	)
}

fn run_directory(directory: &Path) -> PathBuf {
	let mut runs = std::fs::read_dir(directory.join("runs"))
		.unwrap()
		.map(|entry| entry.unwrap().path())
		.collect::<Vec<_>>();
	assert_eq!(runs.len(), 1, "expected one run directory");
	runs.pop().unwrap()
}

#[test]
fn human_run_failure_prints_the_reason_on_standard_error() {
	let directory = work_directory("human");
	let (code, stdout, stderr) = run(&directory, "human");
	let run = run_directory(&directory);
	assert_eq!(code, 1);
	assert_eq!(stdout, format!("Run failed. Artifacts: {}\n", run.display()));
	assert_eq!(
		stderr.lines().last().unwrap(),
		format!(
			"Polkameter: run failed: assertion check failed (details in {})",
			run.join("execution.json").display()
		)
	);
	let execution: Value =
		serde_json::from_slice(&std::fs::read(run.join("execution.json")).unwrap()).unwrap();
	assert_eq!(execution["error"], "assertion check failed");
	std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn json_run_failure_keeps_json_on_standard_output_and_the_reason_on_standard_error() {
	let directory = work_directory("json");
	let (code, stdout, stderr) = run(&directory, "json");
	let run = run_directory(&directory);
	assert_eq!(code, 1);
	let events = stdout
		.lines()
		.map(|line| serde_json::from_str(line).unwrap())
		.collect::<Vec<Value>>();
	let last = events.last().unwrap();
	assert_eq!(last["event"], "artifact-written");
	assert_eq!(last["outcome"]["state"], "failed");
	assert_eq!(last["outcome"]["error"], "assertion check failed");
	assert!(events.iter().any(|event| event["event"] == "completed"
		&& event["outcome"]["error"] == "assertion check failed"));
	assert_eq!(
		stderr,
		format!(
			"Polkameter: run failed: assertion check failed (details in {})\n",
			run.join("execution.json").display()
		)
	);
	std::fs::remove_dir_all(directory).unwrap();
}
