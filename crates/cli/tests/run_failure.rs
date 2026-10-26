//! A failed `polkameter run` prints its reason on standard error, keeps exit code 1 and the run
//! directory, and keeps standard output to the usual lines.

use std::{
	net::{TcpListener, TcpStream},
	path::{Path, PathBuf},
	process::{Child, Command},
	time::{Duration, Instant},
};

use serde_json::Value;

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
		.env("POLKAMETER_AGENT_TOKEN", "run-failure-token");
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
			 Polkameter: run failed: assertion check failed (details in {})\n",
			run.join("execution.json").display()
		)
	);
	let execution: Value =
		serde_json::from_slice(&std::fs::read(run.join("execution.json")).unwrap()).unwrap();
	assert_eq!(execution["error"], "assertion check failed");
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
}

#[test]
fn remote_run_failure_names_the_run_directory_on_the_agent() {
	let work = WorkDirectory::new("remote");
	let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
	let bind = format!("127.0.0.1:{port}");
	let _agent = Agent(
		polkameter(work.path())
			.args(["agent", "serve", "--bind", &bind, "--output-root"])
			.arg(work.path().join("remote"))
			.stdout(std::process::Stdio::null())
			.stderr(std::process::Stdio::null())
			.spawn()
			.unwrap(),
	);
	let deadline = Instant::now() + Duration::from_secs(30);
	while TcpStream::connect(&bind).is_err() {
		assert!(Instant::now() < deadline, "the agent did not start listening");
		std::thread::sleep(Duration::from_millis(50));
	}
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
			"Polkameter: remote run failed: assertion check failed (details in {} on the agent)\n",
			run.join("execution.json").display()
		)
	);
}
