//! SIGINT and SIGTERM stop `run`, the remote client and `agent serve` cleanly: teardown runs,
//! the results are written, plugin processes are killed and reaped, and the exit code names the
//! signal. A second signal interrupts teardown.
//!
//! These tests drive the real binaries. They need the example plugin, built next to the CLI by
//! `cargo test --workspace` or `cargo build -p polkameter-example-plugin`; set
//! `POLKAMETER_EXAMPLE_PLUGIN` to use another build. Every wait is for an observable event (a
//! file the plugin writes, a process exit, an open port), never a fixed sleep.
#![cfg(unix)]

use std::{
	io::{BufRead, BufReader},
	net::{TcpListener, TcpStream},
	path::{Path, PathBuf},
	process::{Child, Command, ExitStatus, Stdio},
	time::{Duration, Instant},
};

use serde_json::Value;

/// How long any one wait in these tests may take before the test fails.
const PATIENCE: Duration = Duration::from_secs(60);

const TOKEN: &str = "signals-test-token-0123456789abcdef";

fn plugin_executable() -> PathBuf {
	if let Some(path) = std::env::var_os("POLKAMETER_EXAMPLE_PLUGIN") {
		return path.into();
	}
	let path =
		Path::new(env!("CARGO_BIN_EXE_polkameter")).with_file_name("polkameter-example-plugin");
	assert!(
		path.is_file(),
		"{} is missing: run `cargo build -p polkameter-example-plugin` or set POLKAMETER_EXAMPLE_PLUGIN",
		path.display()
	);
	path
}

/// A plan whose setup waits for a minute and whose teardown runs `teardown` on the plugin.
fn plan(teardown: &str) -> String {
	format!(
		r#"<polkameter-plan xmlns="https://polkameter.dev/schema/plan" version="1" name="Signals" timeout-ms="600000">
  <plugins><plugin id="example" version="0.1.0" protocol="1"/></plugins>
  <setup>
    <step id="slow" use="example.wait" timeout-ms="120000"><input name="milliseconds" value="60000"/></step>
  </setup>
  <teardown>{teardown}</teardown>
</polkameter-plan>
"#
	)
}

/// A teardown step that needs the plugin: it fails if the plugin cannot be used after the stop.
const DOUBLE: &str =
	r#"<step id="cleanup" use="example.double"><input name="value" value="21"/></step>"#;
/// A teardown step that waits for a minute.
const SLOW_TEARDOWN: &str = r#"<step id="cleanup" use="example.wait" timeout-ms="120000"><input name="milliseconds" value="60000"/></step>"#;

/// A temporary directory with an installed example plugin, removed when dropped.
struct Work(PathBuf);

impl Work {
	fn new(name: &str, plan_xml: &str) -> Self {
		let directory = std::env::temp_dir().join(format!(
			"polkameter-signals-{name}-{}-{}",
			std::process::id(),
			std::time::SystemTime::now()
				.duration_since(std::time::UNIX_EPOCH)
				.unwrap()
				.as_nanos()
		));
		std::fs::create_dir_all(&directory).unwrap();
		let work = Self(directory.canonicalize().unwrap());
		std::fs::write(work.0.join("plan.xml"), plan_xml).unwrap();
		let status = work
			.polkameter()
			.arg("plugin")
			.arg("install")
			.arg(plugin_executable())
			.stdout(Stdio::null())
			.status()
			.unwrap();
		assert!(status.success());
		work
	}

	fn polkameter(&self) -> Command {
		let mut command = Command::new(env!("CARGO_BIN_EXE_polkameter"));
		command
			.env("POLKAMETER_PLUGIN_REGISTRY", self.0.join("plugins.json"))
			.env("POLKAMETER_AGENT_TOKEN", TOKEN);
		command
	}

	/// Starts `polkameter run` on the plan, with JSON output.
	fn run(&self, args: &[&str]) -> Process {
		Process::spawn(
			self.polkameter()
				.arg("run")
				.arg(self.0.join("plan.xml"))
				.args(["--format", "json"])
				.args(args),
		)
	}

	/// The only run directory under `root`, once it exists.
	fn run_directory(&self, root: &str) -> PathBuf {
		wait_for("a run directory", || {
			let runs = std::fs::read_dir(self.0.join(root)).ok()?;
			runs.map(|entry| entry.unwrap().path()).find(|path| path.is_dir())
		})
	}
}

impl Drop for Work {
	fn drop(&mut self) {
		let _ = std::fs::remove_dir_all(&self.0);
	}
}

/// A child process that is killed if the test ends first.
struct Process(Child);

impl Process {
	fn spawn(command: &mut Command) -> Self {
		Self(command.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap())
	}

	fn signal(&self, name: &str) {
		signal(self.0.id(), name);
	}

	/// Waits for the process to exit and returns its status and standard error.
	fn wait(&mut self) -> (ExitStatus, String) {
		let status = wait_for("the process to exit", || self.0.try_wait().unwrap());
		let mut stderr = String::new();
		std::io::Read::read_to_string(self.0.stderr.as_mut().unwrap(), &mut stderr).unwrap();
		(status, stderr)
	}
}

impl Drop for Process {
	fn drop(&mut self) {
		let _ = self.0.kill();
		let _ = self.0.wait();
	}
}

fn signal(pid: u32, name: &str) {
	let status = Command::new("kill").args(["-s", name, &pid.to_string()]).status().unwrap();
	assert!(status.success(), "could not send {name} to {pid}");
}

fn alive(pid: &str) -> bool {
	Command::new("kill")
		.args(["-0", pid])
		.stderr(Stdio::null())
		.status()
		.unwrap()
		.success()
}

/// Polls `ready` until it yields a value, failing the test after [`PATIENCE`].
fn wait_for<T>(what: &str, mut ready: impl FnMut() -> Option<T>) -> T {
	let deadline = Instant::now() + PATIENCE;
	loop {
		if let Some(value) = ready() {
			return value;
		}
		assert!(Instant::now() < deadline, "timed out waiting for {what}");
		std::thread::sleep(Duration::from_millis(10));
	}
}

/// The process ID the plugin wrote when it entered `example.wait`, once it differs from `not`.
fn waiting_plugin(run: &Path, not: Option<&str>) -> String {
	let marker = run.join("plugins/example/wait.pid");
	wait_for("the plugin to start waiting", || {
		let pid = std::fs::read_to_string(&marker).ok()?;
		(!pid.is_empty() && Some(pid.as_str()) != not).then_some(pid)
	})
}

fn read_json(path: &Path) -> Value {
	serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap()
}

fn events(run: &Path) -> Vec<Value> {
	std::fs::read_to_string(run.join("events.jsonl"))
		.unwrap()
		.lines()
		.map(|line| serde_json::from_str(line).unwrap())
		.collect()
}

fn cleanup_succeeded(run: &Path) -> bool {
	events(run).iter().any(|event| {
		event["event"] == "step-finished" && event["step"] == "cleanup" && event["success"] == true
	})
}

/// Checks a stopped local run: exit code, outcome, results, teardown and plugin processes.
fn stopped_with_teardown(signal_name: &str, code: i32) {
	let work = Work::new(&signal_name.to_lowercase(), &plan(DOUBLE));
	let output = work.0.join("runs");
	let mut process = work.run(&["--output", output.to_str().unwrap()]);
	let run = work.run_directory("runs");
	let pid = waiting_plugin(&run, None);
	process.signal(signal_name);
	let (status, stderr) = process.wait();
	assert_eq!(status.code(), Some(code), "{stderr}");
	assert!(
		stderr.contains(&format!("Polkameter: received SIG{signal_name}; stopping the run")),
		"{stderr}"
	);
	let execution = read_json(&run.join("execution.json"));
	assert_eq!(execution["state"], "stopped");
	assert_eq!(execution["exit_code"], code);
	for file in ["summary.md", "junit.xml", "samples.jtl"] {
		assert!(run.join(file).is_file(), "missing {file}");
	}
	// The cancelled call killed the plugin; teardown started it again and used it.
	assert!(events(&run).iter().any(|event| event["event"] == "plugin-restarted"));
	assert!(cleanup_succeeded(&run));
	assert!(!alive(pid.trim()), "the plugin outlived the run");
	let stdout = BufReader::new(process.0.stdout.take().unwrap());
	let last: Value = serde_json::from_str(&stdout.lines().last().unwrap().unwrap()).unwrap();
	assert_eq!(last["event"], "artifact-written");
	assert_eq!(last["outcome"]["exit_code"], code);
}

#[test]
fn sigterm_stops_a_local_run_after_teardown_and_exits_143() {
	stopped_with_teardown("TERM", 143);
}

#[test]
fn sigint_stops_a_local_run_after_teardown_and_exits_130() {
	stopped_with_teardown("INT", 130);
}

/// Starts a local run whose teardown waits, stops it once, and returns it with the run directory
/// and both plugin process IDs once teardown is waiting.
fn stopped_in_teardown(work: &Work, args: &[&str]) -> (Process, PathBuf, String, String) {
	let output = work.0.join("runs");
	let mut all = vec!["--output", output.to_str().unwrap()];
	all.extend(args);
	let process = work.run(&all);
	let run = work.run_directory("runs");
	let first = waiting_plugin(&run, None);
	process.signal("TERM");
	let second = waiting_plugin(&run, Some(&first));
	(process, run, first, second)
}

#[test]
fn a_second_signal_interrupts_teardown() {
	let work = Work::new("second", &plan(SLOW_TEARDOWN));
	let (mut process, run, first, second) = stopped_in_teardown(&work, &[]);
	let started = Instant::now();
	process.signal("TERM");
	let (status, stderr) = process.wait();
	assert!(started.elapsed() < Duration::from_secs(30), "the second signal did not stop the run");
	assert_eq!(status.code(), Some(143), "{stderr}");
	assert!(stderr.contains("Polkameter: stopping now; teardown is interrupted"), "{stderr}");
	let execution = read_json(&run.join("execution.json"));
	assert_eq!(execution["state"], "stopped");
	assert_eq!(execution["exit_code"], 143);
	assert!(
		execution["error"]
			.as_str()
			.unwrap()
			.ends_with("; teardown failed: teardown was interrupted by a forced stop"),
		"{execution}"
	);
	assert!(run.join("junit.xml").is_file());
	assert!(!cleanup_succeeded(&run));
	assert!(!alive(first.trim()) && !alive(second.trim()), "a plugin outlived the run");
}

#[test]
fn the_grace_period_bounds_teardown_after_one_signal() {
	let work = Work::new("grace", &plan(SLOW_TEARDOWN));
	let (mut process, run, first, second) = stopped_in_teardown(&work, &["--shutdown-grace", "1"]);
	let (status, stderr) = process.wait();
	assert_eq!(status.code(), Some(143), "{stderr}");
	assert!(stderr.contains("Polkameter: stopping now; teardown is interrupted"), "{stderr}");
	let execution = read_json(&run.join("execution.json"));
	assert_eq!(
		(execution["state"].as_str(), execution["exit_code"].as_i64()),
		(Some("stopped"), Some(143))
	);
	assert!(!alive(first.trim()) && !alive(second.trim()), "a plugin outlived the run");
}

/// A running `polkameter agent serve` on a free loopback port.
struct Agent {
	process: Process,
	endpoint: String,
}

impl Agent {
	fn start(work: &Work) -> Self {
		let port = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
		let bind = format!("127.0.0.1:{port}");
		let process = Process::spawn(
			work.polkameter()
				.args(["agent", "serve", "--bind", &bind, "--output-root"])
				.arg(work.0.join("remote")),
		);
		wait_for("the agent to listen", || TcpStream::connect(&bind).ok());
		Self { process, endpoint: format!("http://{bind}") }
	}

	fn client(&self, work: &Work) -> Process {
		work.run(&["--remote", &self.endpoint, "--remote-token-env", "POLKAMETER_AGENT_TOKEN"])
	}
}

#[test]
fn sigterm_stops_the_remote_client_which_stops_the_agent_run() {
	let work = Work::new("client", &plan(DOUBLE));
	let mut agent = Agent::start(&work);
	let mut client = agent.client(&work);
	let run = work.run_directory("remote");
	let pid = waiting_plugin(&run, None);
	client.signal("TERM");
	let (status, stderr) = client.wait();
	assert_eq!(status.code(), Some(143), "{stderr}");
	assert!(stderr.contains("received SIGTERM; asking the agent to stop the run"), "{stderr}");
	// The agent stopped the run on request and is free for the next one.
	let execution = read_json(&run.join("execution.json"));
	assert_eq!(
		(execution["state"].as_str(), execution["exit_code"].as_i64()),
		(Some("stopped"), Some(130))
	);
	assert!(cleanup_succeeded(&run));
	assert!(!alive(pid.trim()));
	assert!(agent.process.0.try_wait().unwrap().is_none(), "the agent exited");
	let mut next = agent.client(&work);
	let second = wait_for("a second run directory", || {
		std::fs::read_dir(work.0.join("remote"))
			.unwrap()
			.map(|e| e.unwrap().path())
			.find(|p| p != &run)
	});
	waiting_plugin(&second, None);
	next.signal("INT");
	assert_eq!(next.wait().0.code(), Some(130));
}

#[test]
fn sigterm_stops_the_agent_after_its_run_tears_down() {
	let work = Work::new("agent", &plan(DOUBLE));
	let mut agent = Agent::start(&work);
	let mut client = agent.client(&work);
	let run = work.run_directory("remote");
	let pid = waiting_plugin(&run, None);
	agent.process.signal("TERM");
	let (status, stderr) = agent.process.wait();
	assert_eq!(status.code(), Some(143), "{stderr}");
	assert!(stderr.contains("received SIGTERM; stopping the active run"), "{stderr}");
	let execution = read_json(&run.join("execution.json"));
	assert_eq!(
		(execution["state"].as_str(), execution["exit_code"].as_i64()),
		(Some("stopped"), Some(143))
	);
	assert!(cleanup_succeeded(&run));
	assert!(!alive(pid.trim()), "the plugin outlived the agent");
	// The client read the outcome before the agent closed, and reports the agent's stop.
	let (status, stderr) = client.wait();
	assert_eq!(status.code(), Some(143), "{stderr}");
}
