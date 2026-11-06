//! The CLI's errors in CI: `--format json` prints errors as JSON lines and versions every line,
//! plan errors name the file and position, a closed standard output does not panic, and
//! concurrent `polkameter plugin` commands keep every change to the registry.

use std::{
	io::Read as _,
	path::{Path, PathBuf},
	process::{Command, Stdio},
};

use serde_json::Value;

/// A plan that needs no plugin and no chain.
const CORE_PLAN: &str = r#"<polkameter-plan xmlns="https://polkameter.dev/schema/plan" version="1" name="Core">
  <setup>
    <step id="seed" use="core.echo"><input name="value" value="21"/></step>
    <step id="check" use="core.assert-equal"><input name="actual" ref="steps.seed.value"/><input name="expected" value="21"/></step>
  </setup>
</polkameter-plan>
"#;

/// A temporary directory, removed when dropped, even if a test panics.
struct Work(PathBuf);

impl Work {
	fn new(name: &str) -> Self {
		let nanos = std::time::SystemTime::now()
			.duration_since(std::time::UNIX_EPOCH)
			.unwrap()
			.as_nanos();
		let directory = std::env::temp_dir()
			.join(format!("polkameter-cli-errors-{name}-{}-{nanos}", std::process::id()));
		std::fs::create_dir_all(&directory).unwrap();
		Self(directory.canonicalize().unwrap())
	}

	fn write(&self, name: &str, contents: &str) -> PathBuf {
		let path = self.0.join(name);
		std::fs::write(&path, contents).unwrap();
		path
	}

	fn registry(&self) -> PathBuf {
		self.0.join("config/plugins.json")
	}

	fn polkameter(&self) -> Command {
		let mut command = Command::new(env!("CARGO_BIN_EXE_polkameter"));
		command.env("POLKAMETER_PLUGIN_REGISTRY", self.registry()).current_dir(&self.0);
		command
	}
}

impl Drop for Work {
	fn drop(&mut self) {
		let _ = std::fs::remove_dir_all(&self.0);
	}
}

/// Exit code, standard output and standard error.
fn output(command: &mut Command) -> (i32, String, String) {
	let output = command.output().unwrap();
	(
		output.status.code().unwrap_or(-1),
		String::from_utf8(output.stdout).unwrap(),
		String::from_utf8(output.stderr).unwrap(),
	)
}

/// Every line of `stdout` as JSON, each checked for `event` and `schemaVersion`.
fn json_lines(stdout: &str) -> Vec<Value> {
	let lines: Vec<Value> = stdout
		.lines()
		.map(|line| serde_json::from_str(line).unwrap_or_else(|e| panic!("{line}: {e}")))
		.collect();
	assert!(!lines.is_empty(), "no JSON output");
	for line in &lines {
		assert!(line["event"].is_string(), "line without event: {line}");
		assert_eq!(line["schemaVersion"], 1, "line without schemaVersion: {line}");
	}
	lines
}

#[test]
fn failed_validate_prints_a_json_error_naming_the_file_and_position() {
	let work = Work::new("validate");
	let cases = [
		(
			"nul.xml",
			"<polkameter-plan\n  name=\"\0\"/>",
			2,
			9,
			"character U+0000 is not allowed in XML",
		),
		(
			"comment.xml",
			"<!-- a -- b -->\n<polkameter-plan/>",
			1,
			8,
			"`--` is not allowed inside a comment",
		),
		(
			"declaration.xml",
			"<?xml version=\"1.0\" encoding=\"latin1\"?><polkameter-plan/>",
			1,
			31,
			"invalid encoding `latin1` in the XML declaration",
		),
		(
			"reference.xml",
			&CORE_PLAN.replace("steps.seed.value", "steps.later.value"),
			4,
			46,
			"step `check` (core.assert-equal): input `actual` refers to `steps.later.value`, but \
			 nothing in the plan defines it",
		),
	];
	for (name, xml, line, column, message) in cases {
		let path = work.write(name, xml);
		let (code, stdout, stderr) =
			output(work.polkameter().args(["validate", name, "--format", "json"]));
		assert_eq!(code, 2, "{stderr}");
		let prefix = format!("PolkaMeter: {name}: line {line}, column {column}: {message}");
		assert!(stderr.starts_with(&prefix), "{stderr}");
		let lines = json_lines(&stdout);
		assert_eq!(lines.len(), 1, "{stdout}");
		let error = &lines[0];
		assert_eq!(error["event"], "error");
		assert_eq!(error["command"], "validate");
		assert_eq!(error["kind"], "invalid");
		assert_eq!(error["exitCode"], 2);
		assert_eq!(error["valid"], false);
		assert_eq!(error["file"], name, "{error}");
		assert_eq!((error["line"].as_u64(), error["column"].as_u64()), (Some(line), Some(column)));
		assert!(error["message"].as_str().unwrap().contains(message), "{error}");
		assert!(path.is_file());
	}
}

#[test]
fn a_plan_over_the_size_limit_is_refused_before_parsing() {
	let work = Work::new("size");
	let big = format!("{CORE_PLAN}<!--{}-->", "x".repeat(2 * 1024 * 1024));
	work.write("big.xml", &big);
	let (code, stdout, stderr) =
		output(work.polkameter().args(["validate", "big.xml", "--format", "json"]));
	assert_eq!(code, 2);
	assert!(
		stderr.contains("plan big.xml is larger than the 2 MiB (2097152 bytes) limit"),
		"{stderr}"
	);
	assert_eq!(json_lines(&stdout)[0]["file"], "big.xml");
}

#[cfg(unix)]
#[test]
fn a_fifo_or_directory_plan_is_refused_without_hanging() {
	let work = Work::new("fifo");
	let fifo = work.0.join("plan.xml");
	assert!(Command::new("mkfifo").arg(&fifo).status().unwrap().success());
	let mut child = work
		.polkameter()
		.args(["validate", "plan.xml"])
		.stdout(Stdio::null())
		.stderr(Stdio::piped())
		.spawn()
		.unwrap();
	let started = std::time::Instant::now();
	let status = loop {
		if let Some(status) = child.try_wait().unwrap() {
			break status;
		}
		if started.elapsed() > std::time::Duration::from_secs(20) {
			let _ = child.kill();
			panic!("validate blocked on a FIFO");
		}
		std::thread::sleep(std::time::Duration::from_millis(20));
	};
	let mut stderr = String::new();
	child.stderr.take().unwrap().read_to_string(&mut stderr).unwrap();
	assert_eq!(status.code(), Some(2), "{stderr}");
	assert!(stderr.contains("cannot read plan plan.xml: it is not a regular file"), "{stderr}");
	let (code, _, stderr) = output(work.polkameter().args(["validate", "."]));
	assert_eq!(code, 2);
	assert!(stderr.contains("cannot read plan .: it is a directory"), "{stderr}");
}

#[test]
fn preflight_json_carries_event_and_version_and_errors_are_json() {
	let work = Work::new("preflight");
	work.write("core.xml", CORE_PLAN);
	let (code, stdout, stderr) =
		output(work.polkameter().args(["preflight", "core.xml", "--format", "json"]));
	assert_eq!(code, 0, "{stderr}");
	let lines = json_lines(&stdout);
	assert_eq!(lines.len(), 1);
	assert_eq!(lines[0]["event"], "preflight");
	assert_eq!(lines[0]["valid"], true);

	let missing = CORE_PLAN.replace(
		"<setup>",
		"<plugins><plugin id=\"example\" version=\"0.1.0\" protocol=\"1\"/></plugins><setup>",
	);
	work.write("missing.xml", &missing);
	let (code, stdout, stderr) =
		output(work.polkameter().args(["preflight", "missing.xml", "--format", "json"]));
	assert_eq!(code, 3, "{stderr}");
	let expected = "plugin example is not installed on this host; run `polkameter plugin install \
	                <path-to-executable>`";
	assert!(stderr.contains(expected), "{stderr}");
	let error = &json_lines(&stdout)[0];
	assert_eq!(error["event"], "error");
	assert_eq!(error["command"], "preflight");
	assert_eq!(error["kind"], "preflight");
	assert_eq!(error["exitCode"], 3);
	assert!(error["message"].as_str().unwrap().contains(expected), "{error}");
	assert!(error.get("valid").is_none());

	// A contract error names the file, the step, the operation and the input.
	let unknown = CORE_PLAN.replace("name=\"expected\"", "name=\"wanted\"");
	work.write("unknown.xml", &unknown);
	let (code, stdout, stderr) =
		output(work.polkameter().args(["preflight", "unknown.xml", "--format", "json"]));
	assert_eq!(code, 3, "{stderr}");
	assert!(
		stderr.contains(
			"PolkaMeter: unknown.xml: line 4, column 91: step `check` (core.assert-equal): input \
			 `wanted` is not an input of core.assert-equal; its inputs are `actual`, `expected`"
		),
		"{stderr}"
	);
	let error = &json_lines(&stdout)[0];
	assert_eq!((error["file"].as_str(), error["line"].as_u64()), (Some("unknown.xml"), Some(4)));
}

#[test]
fn run_json_lines_all_carry_event_and_version() {
	let work = Work::new("run");
	work.write("core.xml", CORE_PLAN);
	let (code, stdout, stderr) = output(
		work.polkameter()
			.args(["run", "core.xml", "--output", "runs", "--format", "json"]),
	);
	assert_eq!(code, 0, "{stderr}");
	let lines = json_lines(&stdout);
	let last = lines.last().unwrap();
	assert_eq!(last["event"], "artifact-written");
	assert_eq!(last["outcome"]["state"], "completed");
	// The outcome keeps the keys of execution.json.
	assert_eq!(last["outcome"]["exit_code"], 0);
	assert!(lines.iter().any(|line| line["event"] == "step-finished"));
}

/// Runs `args` with standard output closed by the reader before the command writes, and returns
/// its exit code and standard error.
fn with_closed_stdout(work: &Work, args: &[&str]) -> (i32, String) {
	let mut child = work
		.polkameter()
		.args(args)
		.stdout(Stdio::piped())
		.stderr(Stdio::piped())
		.spawn()
		.unwrap();
	drop(child.stdout.take());
	let output = child.wait_with_output().unwrap();
	(output.status.code().unwrap_or(-1), String::from_utf8(output.stderr).unwrap())
}

#[test]
fn a_closed_stdout_does_not_panic() {
	let work = Work::new("pipe");
	work.write("core.xml", CORE_PLAN);
	for (args, expected) in [
		(&["validate", "core.xml"][..], 0),
		(&["validate", "core.xml", "--format", "json"], 0),
		(&["validate", "missing.xml", "--format", "json"], 2),
		(&["run", "core.xml", "--output", "runs", "--format", "json"], 0),
		(&["run", "core.xml", "--output", "runs"], 0),
		(&["plugin", "list"], 0),
		(&["--help"], 0),
	] {
		let (code, stderr) = with_closed_stdout(&work, args);
		assert!(!stderr.contains("panicked"), "{args:?}: {stderr}");
		assert_eq!(code, expected, "{args:?}: {stderr}");
	}
	// The run still wrote its results.
	assert_eq!(std::fs::read_dir(work.0.join("runs")).unwrap().count(), 2);
}

#[test]
fn concurrent_registry_writers_keep_every_change() {
	let work = Work::new("registry");
	let count = 12;
	let children: Vec<_> = (0..count)
		.map(|i| {
			let mut command = work.polkameter();
			if i % 2 == 0 {
				command.args([
					"plugin",
					"credential",
					&format!("profile-{i}"),
					&format!("ENV_{i}"),
				]);
			} else {
				command.args(["plugin", "capability", &format!("capability-{i}")]);
			}
			command.stdout(Stdio::null()).stderr(Stdio::piped()).spawn().unwrap()
		})
		.collect();
	for child in children {
		let output = child.wait_with_output().unwrap();
		assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
	}
	let registry: Value = serde_json::from_slice(&std::fs::read(work.registry()).unwrap()).unwrap();
	for i in 0..count {
		if i % 2 == 0 {
			assert_eq!(registry["credentials"][format!("profile-{i}")], format!("ENV_{i}"));
		} else {
			let capabilities = registry["capabilities"].as_array().unwrap();
			assert!(capabilities.contains(&Value::from(format!("capability-{i}"))), "{registry}");
		}
	}
	// No temporary file is left next to the registry.
	let leftovers: Vec<_> = std::fs::read_dir(work.registry().parent().unwrap())
		.unwrap()
		.map(|entry| entry.unwrap().file_name().into_string().unwrap())
		.filter(|name| name.ends_with(".tmp"))
		.collect();
	assert!(leftovers.is_empty(), "{leftovers:?}");
}

/// A plugin executable that answers `$describe` with plugin `id`.
#[cfg(unix)]
fn script_plugin(directory: &Path, id: &str) -> PathBuf {
	use std::os::unix::fs::PermissionsExt;
	let manifest = format!(r#"{{"id":"{id}","version":"0.1.0","protocol":1,"operations":{{}}}}"#);
	let path = directory.join(id);
	std::fs::write(
		&path,
		format!(
			"#!/bin/sh\nread request\nprintf '%s\\n' '{{\"protocol\":1,\"id\":1,\"result\":{manifest}}}'\n"
		),
	)
	.unwrap();
	std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
	path
}

#[cfg(unix)]
#[test]
fn concurrent_installs_keep_every_plugin() {
	let work = Work::new("installs");
	let ids: Vec<_> = (0..8).map(|i| format!("plugin{i}")).collect();
	let children: Vec<_> = ids
		.iter()
		.map(|id| {
			let executable = script_plugin(&work.0, id);
			work.polkameter()
				.arg("plugin")
				.arg("install")
				.arg(executable)
				.stdout(Stdio::piped())
				.stderr(Stdio::piped())
				.spawn()
				.unwrap()
		})
		.collect();
	for child in children {
		let output = child.wait_with_output().unwrap();
		assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
		assert!(String::from_utf8_lossy(&output.stdout).starts_with("Installed plugin plugin"));
	}
	let registry: Value = serde_json::from_slice(&std::fs::read(work.registry()).unwrap()).unwrap();
	for id in &ids {
		assert_eq!(registry["plugins"][id]["version"], "0.1.0", "{registry}");
	}
}

#[test]
fn a_corrupt_registry_names_its_path_and_the_recovery() {
	let work = Work::new("corrupt");
	std::fs::create_dir_all(work.registry().parent().unwrap()).unwrap();
	std::fs::write(work.registry(), "{\"plugins\": ").unwrap();
	let (code, _, stderr) = output(work.polkameter().args(["plugin", "list"]));
	assert_eq!(code, 2);
	let path = work.registry();
	assert!(
		stderr.contains(&format!("plugin registry {} is not a valid registry:", path.display())),
		"{stderr}"
	);
	assert!(stderr.contains("move it aside and run `polkameter plugin install`"), "{stderr}");
	// A write does not replace the corrupt file.
	let (code, _, _) = output(work.polkameter().args(["plugin", "capability", "x"]));
	assert_eq!(code, 2);
	assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"plugins\": ");
}
