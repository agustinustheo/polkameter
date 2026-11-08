//! A failed `polkameter run` prints its reason on standard error, keeps exit code 1 and the run
//! directory, and keeps standard output to the usual lines.

use std::{
	path::{Path, PathBuf},
	process::Command,
};

use serde_json::Value;

#[path = "support/work.rs"]
mod work;

use work::Work;

const FAILING_PLAN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<polkameter-plan xmlns="https://polkameter.dev/schema/plan" version="1" name="Failing" timeout-ms="30000">
  <setup>
    <step id="check" use="core.assert-equal"><input name="actual" value="1"/><input name="expected" value="2"/></step>
  </setup>
</polkameter-plan>
"#;

impl Work {
	/// The only run directory under `root`.
	fn run_directory(&self, root: &str) -> PathBuf {
		let mut runs = std::fs::read_dir(self.0.join(root))
			.unwrap()
			.map(|entry| entry.unwrap().path())
			.collect::<Vec<_>>();
		assert_eq!(runs.len(), 1, "expected one run directory");
		runs.pop().unwrap()
	}
}

fn polkameter(directory: &Path) -> Command {
	let mut command = Command::new(env!("CARGO_BIN_EXE_polkameter"));
	command.env("POLKAMETER_PLUGIN_REGISTRY", directory.join("plugins.json"));
	command
}

fn output(command: &mut Command) -> (i32, String, String) {
	let output = command.output().unwrap();
	(
		output.status.code().unwrap(),
		String::from_utf8(output.stdout).unwrap(),
		String::from_utf8(output.stderr).unwrap(),
	)
}

fn run(directory: &Path) -> (i32, String, String) {
	output(
		polkameter(directory)
			.arg("run")
			.arg(directory.join("plan.xml"))
			.arg("--output")
			.arg(directory.join("runs")),
	)
}

#[test]
fn a_failed_run_exits_1_with_one_clean_reason_line() {
	let work = Work::new("run-failure");
	// The actual value is a JSON string of control characters; none may reach the terminal raw.
	let plan = FAILING_PLAN.replace(
		r#"name="actual" value="1""#,
		r#"name="actual" value="&quot;\n\t\u001b\u202e&quot;""#,
	);
	std::fs::write(work.0.join("plan.xml"), plan).unwrap();
	let (code, stdout, stderr) = run(&work.0);
	assert_eq!(code, 1, "{stderr}");
	assert!(stdout.starts_with("Run failed. Artifacts: "), "{stdout}");
	let reasons = stderr.lines().filter(|line| !line.starts_with("Phase: "));
	assert_eq!(reasons.count(), 1, "{stderr}");
	assert!(!stderr.contains(['\t', '\u{1b}', '\u{202e}']), "{stderr}");
	let execution: Value = serde_json::from_slice(
		&std::fs::read(work.run_directory("runs").join("execution.json")).unwrap(),
	)
	.unwrap();
	assert_eq!(execution["state"], "failed");
}

#[test]
fn a_run_that_fails_before_setup_names_the_plan_position_and_exits_3() {
	let work = Work::new("run-failure-contract");
	// `core.echo` has no input `valeu`; the run finds this before its setup phase.
	let plan = FAILING_PLAN.replace(
		r#"<step id="check" use="core.assert-equal"><input name="actual" value="1"/><input name="expected" value="2"/></step>"#,
		r#"<step id="check" use="core.echo"><input name="valeu" value="1"/></step>"#,
	);
	std::fs::write(work.0.join("plan.xml"), plan).unwrap();
	let (code, _, stderr) = run(&work.0);
	let run_dir = work.run_directory("runs");
	assert_eq!(code, 3, "{stderr}");
	let plan_path = work.0.join("plan.xml");
	let execution: Value =
		serde_json::from_slice(&std::fs::read(run_dir.join("execution.json")).unwrap()).unwrap();
	assert_eq!(execution["exit_code"], 3);
	assert_eq!(execution["state"], "failed");
	assert_eq!(execution["failed_phase"], "preflight");
	assert_eq!(execution["error_location"], serde_json::json!({"line": 4, "column": 38}));
	assert!(execution.get("teardown").is_none());
	let reason = format!("run failed before setup: {}: line 4, column 38: ", plan_path.display());
	assert!(stderr.contains(&reason), "{stderr}");
	assert!(!stderr.contains("Phase: teardown"), "{stderr}");
}
