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
			"<?xml version=\"1.0\" encoding=\"UTF-16\"?><polkameter-plan/>",
			1,
			31,
			"invalid encoding `UTF-16` in the XML declaration",
		),
		(
			"reference.xml",
			&CORE_PLAN.replace("steps.seed.value", "steps.later.value"),
			4,
			46,
			"step `check` (core.assert-equal): input `actual` refers to `steps.later.value`, but \
			 nothing in the plan defines it",
		),
		(
			// A built-in operation's contract is checked without plugins.
			"core-input.xml",
			&CORE_PLAN.replace("name=\"expected\"", "name=\"expectd\""),
			4,
			91,
			"step `check` (core.assert-equal): input `expectd` is not an input of \
			 core.assert-equal; did you mean `expected`? its inputs are `actual`, `expected`",
		),
		(
			"core-op.xml",
			&CORE_PLAN.replace("core.echo", "core.ecco"),
			3,
			5,
			"step `seed` (core.ecco): unknown built-in operation core.ecco; did you mean \
			 core.echo? the core operations are core.echo, core.assert-equal, \
			 core.submit-prepared, core.submit-setup",
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
fn text_that_is_not_utf8_has_a_json_position() {
	let work = Work::new("latin1");
	std::fs::write(work.0.join("latin1.xml"), b"<polkameter-plan>\n  caf\xe9").unwrap();
	let (code, stdout, stderr) =
		output(work.polkameter().args(["validate", "latin1.xml", "--format", "json"]));
	assert_eq!(code, 2, "{stderr}");
	assert!(
		stderr.starts_with(
			"PolkaMeter: latin1.xml: line 2, column 6: the plan is not UTF-8 text; save it as UTF-8"
		),
		"{stderr}"
	);
	let error = &json_lines(&stdout)[0];
	assert_eq!((error["line"].as_u64(), error["column"].as_u64()), (Some(2), Some(6)));
	assert_eq!(error["file"], "latin1.xml");
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
	// Nothing writes to the FIFO, so it reads as empty.
	assert_eq!(status.code(), Some(2), "{stderr}");
	assert!(stderr.contains("plan plan.xml is empty"), "{stderr}");
	let (code, _, stderr) = output(work.polkameter().args(["validate", "."]));
	assert_eq!(code, 2);
	assert!(stderr.contains("cannot read plan .: it is a directory"), "{stderr}");
	let (code, _, stderr) = output(work.polkameter().args(["validate", "/dev/null"]));
	assert_eq!(code, 2);
	assert!(stderr.contains("cannot read plan /dev/null: it is a socket or device"), "{stderr}");
}

#[cfg(unix)]
#[test]
fn a_plan_from_a_pipe_or_process_substitution_is_read() {
	use std::io::Write as _;
	let work = Work::new("pipe-input");
	work.write("core.xml", CORE_PLAN);
	// `cat plan | polkameter validate /dev/stdin`
	let mut child = work
		.polkameter()
		.args(["validate", "/dev/stdin"])
		.stdin(Stdio::piped())
		.stdout(Stdio::piped())
		.stderr(Stdio::piped())
		.spawn()
		.unwrap();
	let mut stdin = child.stdin.take().unwrap();
	stdin.write_all(CORE_PLAN.as_bytes()).unwrap();
	drop(stdin);
	let result = child.wait_with_output().unwrap();
	let stderr = String::from_utf8_lossy(&result.stderr);
	assert_eq!(result.status.code(), Some(0), "{stderr}");
	assert!(String::from_utf8_lossy(&result.stdout).starts_with("Plan is structurally valid"));
	// `polkameter validate <(cat plan)`, where bash is available.
	if Command::new("bash").arg("-c").arg("true").status().is_ok_and(|s| s.success()) {
		let script = format!("\"{}\" validate <(cat core.xml)", env!("CARGO_BIN_EXE_polkameter"));
		let (code, _, stderr) =
			output(Command::new("bash").arg("-c").arg(script).current_dir(&work.0));
		assert_eq!(code, 0, "{stderr}");
		// Errors still name the position.
		let broken =
			format!("\"{}\" validate <(printf '<a>\\n</b>')", env!("CARGO_BIN_EXE_polkameter"));
		let (code, _, stderr) =
			output(Command::new("bash").arg("-c").arg(broken).current_dir(&work.0));
		assert_eq!(code, 2, "{stderr}");
		assert!(stderr.contains("line 2, column 3: expected </a>, found </b>"), "{stderr}");
	}
}

#[cfg(unix)]
#[test]
fn a_path_that_is_not_utf8_is_an_input_error_in_json() {
	use std::os::unix::ffi::OsStrExt as _;
	let work = Work::new("non-utf8");
	let name = std::ffi::OsStr::from_bytes(b"plan-\xff.xml");
	let (code, stdout, stderr) =
		output(work.polkameter().arg("validate").arg(name).args(["--format", "json"]));
	assert_eq!(code, 2, "{stderr}");
	assert!(!stderr.contains("panicked"), "{stderr}");
	let error = &json_lines(&stdout)[0];
	assert_eq!(error["file"], "plan-\u{fffd}.xml");
	work.write("core.xml", CORE_PLAN);
	let (code, _, stderr) = output(
		work.polkameter()
			.args(["run", "core.xml", "--output"])
			.arg(name)
			.args(["--format", "json"]),
	);
	assert_eq!(code, 2, "{stderr}");
	assert!(stderr.contains("is not a UTF-8 path"), "{stderr}");
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
	// It names the <plugin> element and the registry it read.
	assert_eq!((error["file"].as_str(), error["line"].as_u64()), (Some("missing.xml"), Some(2)));
	let registry = format!("(registry {})", work.registry().display());
	assert!(stderr.contains(&registry), "{stderr}");

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

/// A plan with one credential, used by a core step so that a run needs its value.
const CREDENTIAL_PLAN: &str = r#"<polkameter-plan xmlns="https://polkameter.dev/schema/plan" version="1" name="Credential">
  <credentials><credential id="sudo" profile="admin"/></credentials>
  <setup>
    <step id="seed" use="core.echo"><input name="value" ref="credentials.sudo"/></step>
  </setup>
</polkameter-plan>
"#;

#[test]
fn credential_errors_name_the_variable_and_how_to_set_it() {
	let work = Work::new("credentials");
	work.write("plan.xml", CREDENTIAL_PLAN);
	let variable = "POLKAMETER_CLI_TEST_SECRET";
	// Not mapped: preflight and run name the profile, the command that maps it and the registry.
	let registry = work.registry();
	for args in [&["preflight", "plan.xml"][..], &["run", "plan.xml", "--output", "runs"]] {
		let (code, _, stderr) = output(work.polkameter().args(args).env_remove(variable));
		assert_eq!(code, 3, "{args:?}: {stderr}");
		assert!(
			stderr.contains(
				"plan.xml: line 2, column 16: credential `sudo` uses profile `admin`, which is not \
				 configured on this host; run `polkameter plugin credential admin <ENV_VAR>` to \
				 name the environment variable that holds it"
			),
			"{args:?}: {stderr}"
		);
		// The run's one-line reason may cut the registry path; the fix comes before it.
		if args[0] == "preflight" {
			assert!(stderr.contains(&format!("(registry {})", registry.display())), "{stderr}");
		}
	}
	let (code, _, stderr) =
		output(work.polkameter().args(["plugin", "credential", "admin", variable]));
	assert_eq!(code, 0, "{stderr}");
	// `plugin list` shows the mapping, never a value.
	let (code, stdout, _) =
		output(work.polkameter().args(["plugin", "list"]).env(variable, "s3cret-value"));
	assert_eq!(code, 0);
	let listed: Value = serde_json::from_str(&stdout).unwrap();
	assert_eq!(listed["credentials"]["admin"], variable);
	assert!(listed["plugins"].is_object(), "{listed}");
	assert!(!stdout.contains("s3cret-value"));
	// Mapped, but unset or empty: preflight checks it as the run would, without a step using it.
	for (value, problem) in [(None, "is not set"), (Some(""), "is empty")] {
		for args in [&["preflight", "plan.xml"][..], &["run", "plan.xml", "--output", "runs"]] {
			let mut command = work.polkameter();
			command.args(args);
			match value {
				Some(value) => command.env(variable, value),
				None => command.env_remove(variable),
			};
			let (code, _, stderr) = output(&mut command);
			assert_eq!(code, 3, "{args:?}: {stderr}");
			let expected = format!(
				"credential `sudo` (profile `admin`) reads environment variable {variable} \
				 (mapped by `polkameter plugin credential`), which {problem}; set {variable} on \
				 the step that runs polkameter"
			);
			assert!(stderr.contains(&expected), "{args:?}: {stderr}");
		}
	}
	// --credential-env names the flag.
	let (code, _, stderr) = output(
		work.polkameter()
			.args([
				"run",
				"plan.xml",
				"--output",
				"runs",
				"--credential-env",
				"UNSET_FLAG_VARIABLE",
			])
			.env_remove("UNSET_FLAG_VARIABLE"),
	);
	assert_eq!(code, 3, "{stderr}");
	assert!(
		stderr.contains(
			"reads environment variable UNSET_FLAG_VARIABLE (from --credential-env), which is not \
			 set; set UNSET_FLAG_VARIABLE on the step that runs polkameter, or pass the variable \
			 that holds it to --credential-env"
		),
		"{stderr}"
	);
	// A run that failed before setup skipped teardown and recorded exit code 3.
	let runs: Vec<_> = std::fs::read_dir(work.0.join("runs")).unwrap().collect();
	for run in runs {
		let execution: Value = serde_json::from_slice(
			&std::fs::read(run.unwrap().path().join("execution.json")).unwrap(),
		)
		.unwrap();
		assert_eq!(
			(execution["state"].as_str(), execution["exit_code"].as_i64()),
			(Some("failed"), Some(3))
		);
		assert!(execution.get("teardown").is_none(), "{execution}");
		assert_eq!(execution["failed_phase"], "preflight");
	}
	// Set: both pass.
	for args in [&["preflight", "plan.xml"][..], &["run", "plan.xml", "--output", "runs"]] {
		let (code, _, stderr) = output(work.polkameter().args(args).env(variable, "s3cret-value"));
		assert_eq!(code, 0, "{args:?}: {stderr}");
		assert!(!stderr.contains("s3cret-value"));
	}
}

/// Decompresses the recorded smoke run into `directory`.
fn smoke_fixture(directory: &Path) {
	let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/smoke-run");
	std::fs::create_dir_all(directory).unwrap();
	for entry in std::fs::read_dir(&fixture).unwrap() {
		let path = entry.unwrap().path();
		if path.extension().is_some_and(|ext| ext == "gz") {
			let mut decoder = flate2::read::GzDecoder::new(std::fs::File::open(&path).unwrap());
			let mut output =
				std::fs::File::create(directory.join(path.file_stem().unwrap())).unwrap();
			std::io::copy(&mut decoder, &mut output).unwrap();
		} else if path.extension().is_some_and(|ext| ext == "json" || ext == "jsonl") {
			std::fs::copy(&path, directory.join(path.file_name().unwrap())).unwrap();
		}
	}
}

/// Runs `args` with standard error closed by its reader before the command writes, and returns
/// the exit code and standard output.
fn with_closed_stderr(work: &Work, args: &[&str]) -> (i32, String) {
	let mut child = work
		.polkameter()
		.args(args)
		.stdout(Stdio::piped())
		.stderr(Stdio::piped())
		.spawn()
		.unwrap();
	drop(child.stderr.take());
	let output = child.wait_with_output().unwrap();
	(output.status.code().unwrap_or(-1), String::from_utf8(output.stdout).unwrap())
}

#[test]
fn a_closed_stderr_does_not_panic_in_report_or_run() {
	let work = Work::new("stderr");
	work.write("core.xml", CORE_PLAN);
	// `report` prints the load summary on standard error, through the SDK.
	smoke_fixture(&work.0.join("smoke"));
	let (code, stdout) = with_closed_stderr(&work, &["report", "smoke"]);
	assert_eq!(code, 0, "{stdout}");
	assert!(stdout.contains("- **Verdict:** FAIL"), "{stdout}");
	let (code, stdout) = with_closed_stderr(&work, &["run", "core.xml", "--output", "runs"]);
	assert_eq!(code, 0, "{stdout}");
	assert!(stdout.starts_with("Run completed."), "{stdout}");
}

#[cfg(target_os = "linux")]
#[test]
fn a_full_disk_on_stdout_is_reported_and_fails_the_command() {
	let work = Work::new("full");
	work.write("core.xml", CORE_PLAN);
	for args in [&["validate", "core.xml"][..], &["run", "core.xml", "--output", "runs"]] {
		let full = std::fs::OpenOptions::new().write(true).open("/dev/full").unwrap();
		let result = work.polkameter().args(args).stdout(full).output().unwrap();
		let stderr = String::from_utf8_lossy(&result.stderr);
		assert_eq!(result.status.code(), Some(1), "{args:?}: {stderr}");
		assert!(stderr.contains("PolkaMeter: cannot write to standard output: "), "{stderr}");
		assert_eq!(stderr.matches("cannot write to standard output").count(), 1, "{stderr}");
	}
	// The run still finished and wrote its results.
	assert_eq!(std::fs::read_dir(work.0.join("runs")).unwrap().count(), 1);
}

#[test]
fn a_registry_writer_says_what_it_waits_for() {
	let work = Work::new("lock-wait");
	std::fs::create_dir_all(work.registry().parent().unwrap()).unwrap();
	let lock_path = work.0.join("config/plugins.json.lock");
	let lock = std::fs::File::create(&lock_path).unwrap();
	lock.lock().unwrap();
	let mut child = work
		.polkameter()
		.args(["plugin", "capability", "waited"])
		.stdout(Stdio::null())
		.stderr(Stdio::piped())
		.spawn()
		.unwrap();
	// Release the lock once the writer says it waits for it.
	let mut stderr = child.stderr.take().unwrap();
	let mut said = Vec::new();
	let mut byte = [0];
	while !said.ends_with(b"\n") && stderr.read(&mut byte).unwrap() == 1 {
		said.push(byte[0]);
	}
	drop(lock);
	let mut rest = String::new();
	stderr.read_to_string(&mut rest).unwrap();
	let output = child.wait_with_output().unwrap();
	let stderr = String::from_utf8_lossy(&said).into_owned() + &rest;
	assert!(output.status.success(), "{stderr}");
	assert!(
		stderr.contains(&format!(
			"PolkaMeter: waiting for {} (another polkameter is updating the registry)",
			lock_path.display()
		)),
		"{stderr}"
	);
	let registry: Value = serde_json::from_slice(&std::fs::read(work.registry()).unwrap()).unwrap();
	assert_eq!(registry["capabilities"][0], "waited");
}
