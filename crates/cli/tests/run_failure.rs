//! A failed `polkameter run` prints its reason on standard error, keeps exit code 1 and the run
//! directory, and keeps standard output to the usual lines.

use std::{
	path::{Path, PathBuf},
	process::{Child, Command},
	time::Duration,
};

use serde_json::Value;

#[path = "support/agent.rs"]
mod agent;

const FAILING_PLAN: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<polkameter-plan xmlns="https://polkameter.dev/schema/plan" version="1" name="Failing" timeout-ms="30000">
  <setup>
    <step id="check" use="core.assert-equal"><input name="actual" value="1"/><input name="expected" value="2"/></step>
  </setup>
</polkameter-plan>
"#;

/// A temporary directory holding the failing plan, removed when dropped, even if a test panics.
struct WorkDirectory(PathBuf);

impl WorkDirectory {
	fn new(name: &str) -> Self {
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
		Self(directory.canonicalize().unwrap())
	}

	fn path(&self) -> &Path {
		&self.0
	}

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

impl Drop for WorkDirectory {
	fn drop(&mut self) {
		let _ = std::fs::remove_dir_all(&self.0);
	}
}

/// A running `polkameter agent serve`, killed when dropped.
struct Agent(Child);

impl Drop for Agent {
	fn drop(&mut self) {
		let _ = self.0.kill();
		let _ = self.0.wait();
	}
}

fn polkameter(directory: &Path) -> Command {
	let mut command = Command::new(env!("CARGO_BIN_EXE_polkameter"));
	command
		.env("POLKAMETER_PLUGIN_REGISTRY", directory.join("plugins.json"))
		.env("POLKAMETER_AGENT_TOKEN", "run-failure-token-0123456789abcdef");
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

fn run(directory: &Path, format: &str) -> (i32, String, String) {
	output(
		polkameter(directory)
			.arg("run")
			.arg(directory.join("plan.xml"))
			.arg("--output")
			.arg(directory.join("runs"))
			.args(["--format", format]),
	)
}

#[test]
fn human_run_failure_prints_the_reason_on_standard_error() {
	let work = WorkDirectory::new("human");
	let (code, stdout, stderr) = run(work.path(), "human");
	let run = work.run_directory("runs");
	assert_eq!(code, 1);
	assert_eq!(stdout, format!("Run failed. Artifacts: {}\n", run.display()));
	assert_eq!(
		stderr,
		format!(
			"Phase: preflight\nPhase: setup\nPhase: teardown\n\
			 PolkaMeter: run failed: assertion check failed: expected 2, got 1 (details in {})\n",
			run.join("execution.json").display()
		)
	);
	let execution: Value =
		serde_json::from_slice(&std::fs::read(run.join("execution.json")).unwrap()).unwrap();
	assert_eq!(execution["error"], "assertion check failed: expected 2, got 1");
}

#[test]
fn assertion_control_differences_survive_stderr_and_recorded_errors() {
	for (actual, rendered) in [
		("\n", r#""\n""#),
		("\t", r#""\t""#),
		("\u{1b}", r#""\u001b""#),
		("\u{202e}", r#""\u202e""#),
	] {
		let work = WorkDirectory::new("controls");
		let actual = serde_json::to_string(actual).unwrap().replace('"', "&quot;");
		let plan = FAILING_PLAN
			.replace(r#"name="actual" value="1""#, &format!(r#"name="actual" value="{actual}""#))
			.replace(r#"name="expected" value="2""#, r#"name="expected" value="&quot; &quot;""#);
		std::fs::write(work.path().join("plan.xml"), plan).unwrap();
		let (code, _, stderr) = run(work.path(), "json");
		assert_eq!(code, 1);
		let expected = format!(r#"assertion check failed: expected " ", got {rendered}"#);
		assert!(stderr.contains(&expected), "{stderr}");
		assert_eq!(stderr.lines().count(), 1);
		assert!(!stderr.contains(['\t', '\u{1b}', '\u{202e}']));
		let execution: Value = serde_json::from_slice(
			&std::fs::read(work.run_directory("runs").join("execution.json")).unwrap(),
		)
		.unwrap();
		assert_eq!(execution["error"], expected);
		let junit = std::fs::read_to_string(work.run_directory("runs").join("junit.xml")).unwrap();
		assert!(junit.contains(&expected.replace('"', "&quot;")), "{junit}");
	}
}

#[test]
fn json_run_failure_keeps_json_on_standard_output_and_the_reason_on_standard_error() {
	let work = WorkDirectory::new("json");
	let (code, stdout, stderr) = run(work.path(), "json");
	let run = work.run_directory("runs");
	assert_eq!(code, 1);
	let events = stdout
		.lines()
		.map(|line| serde_json::from_str(line).unwrap())
		.collect::<Vec<Value>>();
	let last = events.last().unwrap();
	assert_eq!(last["event"], "artifact-written");
	assert_eq!(last["outcome"]["state"], "failed");
	assert_eq!(last["outcome"]["error"], "assertion check failed: expected 2, got 1");
	assert!(events.iter().any(|event| event["event"] == "completed"
		&& event["outcome"]["error"] == "assertion check failed: expected 2, got 1"));
	assert_eq!(
		stderr,
		format!(
			"PolkaMeter: run failed: assertion check failed: expected 2, got 1 (details in {})\n",
			run.join("execution.json").display()
		)
	);
}

#[test]
fn remote_run_failure_names_the_run_directory_on_the_agent() {
	let work = WorkDirectory::new("remote");
	let (child, bind, _stderr) = agent::start(
		polkameter(work.path())
			.args(["agent", "serve", "--output-root"])
			.arg(work.path().join("remote"))
			.stdout(std::process::Stdio::null()),
		Duration::from_secs(120),
	);
	let _agent = Agent(child);
	let (code, stdout, stderr) = output(
		polkameter(work.path())
			.arg("run")
			.arg(work.path().join("plan.xml"))
			.args(["--remote", &format!("http://{bind}")])
			.args(["--remote-token-env", "POLKAMETER_AGENT_TOKEN"]),
	);
	let run = work.run_directory("remote");
	assert_eq!(code, 1);
	assert_eq!(stdout, format!("Remote run failed. Artifacts: {}\n", run.display()));
	assert_eq!(
		stderr,
		format!(
			"PolkaMeter: remote run failed: assertion check failed: expected 2, got 1 (details in {} on the agent)\n",
			run.join("execution.json").display()
		)
	);
}

#[test]
fn a_run_that_fails_before_setup_names_the_plan_position_and_exits_3() {
	let work = WorkDirectory::new("contract");
	// `core.echo` has no input `valeu`; the run finds this before its setup phase.
	let plan = FAILING_PLAN.replace(
		r#"<step id="check" use="core.assert-equal"><input name="actual" value="1"/><input name="expected" value="2"/></step>"#,
		r#"<step id="check" use="core.echo"><input name="valeu" value="1"/></step>"#,
	);
	std::fs::write(work.path().join("plan.xml"), plan).unwrap();
	let (code, _, stderr) = run(work.path(), "human");
	let run_dir = work.run_directory("runs");
	assert_eq!(code, 3, "{stderr}");
	let plan_path = work.path().join("plan.xml");
	assert_eq!(
		stderr,
		format!(
			"Phase: preflight\nPolkaMeter: run failed before setup: {}: line 4, column 38: step \
			 `check` (core.echo): input `valeu` is not an input of core.echo; did you mean \
			 `value`? its inputs are `value` (details in {})\n",
			plan_path.display(),
			run_dir.join("execution.json").display()
		)
	);
	let execution: Value =
		serde_json::from_slice(&std::fs::read(run_dir.join("execution.json")).unwrap()).unwrap();
	assert_eq!(execution["exit_code"], 3);
	assert_eq!(execution["state"], "failed");
	assert_eq!(execution["failed_phase"], "preflight");
	assert_eq!(execution["error_location"], serde_json::json!({"line": 4, "column": 38}));
	assert!(execution.get("teardown").is_none());

	let (code, stdout, _) = run(work.path(), "json");
	assert_eq!(code, 3);
	let last: Value = serde_json::from_str(stdout.lines().last().unwrap()).unwrap();
	assert_eq!(last["event"], "artifact-written");
	assert_eq!(last["file"], plan_path.display().to_string());
	assert_eq!((last["line"].as_u64(), last["column"].as_u64()), (Some(4), Some(38)));
	assert!(!stdout.contains(r#""phase":"teardown""#), "{stdout}");
}
