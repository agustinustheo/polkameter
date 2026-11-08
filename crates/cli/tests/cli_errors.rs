//! The CLI's errors in CI: plan errors name the file and position, a closed standard output does
//! not panic, and concurrent `polkameter plugin` commands keep every change to the registry.

use std::{
	path::{Path, PathBuf},
	process::{Command, Stdio},
};

use serde_json::Value;

#[path = "support/executable.rs"]
mod executable;
#[path = "support/smoke_run.rs"]
mod smoke_run;
#[path = "support/work.rs"]
mod work;

use work::Work;

/// A plan that needs no plugin and no chain.
const CORE_PLAN: &str = r#"<polkameter-plan xmlns="https://polkameter.dev/schema/plan" version="1" name="Core">
  <setup>
    <step id="seed" use="core.echo"><input name="value" value="21"/></step>
    <step id="check" use="core.assert-equal"><input name="actual" ref="steps.seed.value"/><input name="expected" value="21"/></step>
  </setup>
</polkameter-plan>
"#;

impl Work {
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

/// Exit code, standard output and standard error.
fn output(command: &mut Command) -> (i32, String, String) {
	let output = command.output().unwrap();
	(
		output.status.code().unwrap_or(-1),
		String::from_utf8(output.stdout).unwrap(),
		String::from_utf8(output.stderr).unwrap(),
	)
}

#[test]
fn failed_validate_names_the_file_and_position() {
	let work = Work::new("validate");
	let big = format!("{CORE_PLAN}<!--{}-->", "x".repeat(2 * 1024 * 1024));
	// Files left empty here are not written: a directory, a FIFO and a device.
	let mut cases = vec![
		("nul.xml", b"<polkameter-plan\n  name=\"\0\"/>".to_vec(), Some((2, 9))),
		("comment.xml", b"<!-- a -- b -->\n<polkameter-plan/>".to_vec(), Some((1, 8))),
		(
			"declaration.xml",
			b"<?xml version=\"1.0\" encoding=\"UTF-16\"?><polkameter-plan/>".to_vec(),
			Some((1, 31)),
		),
		("latin1.xml", b"<polkameter-plan>\n  caf\xe9".to_vec(), Some((2, 6))),
		(
			"reference.xml",
			CORE_PLAN.replace("steps.seed.value", "steps.later.value").into(),
			Some((4, 46)),
		),
		// A built-in operation's contract is checked without plugins.
		(
			"core-input.xml",
			CORE_PLAN.replace("name=\"expected\"", "name=\"expectd\"").into(),
			Some((4, 91)),
		),
		("core-op.xml", CORE_PLAN.replace("core.echo", "core.ecco").into(), Some((3, 5))),
		// Refused before parsing.
		("big.xml", big.into(), None),
		(".", vec![], None),
	];
	if cfg!(unix) {
		// Nothing writes to the FIFO, so it reads as empty instead of blocking.
		assert!(Command::new("mkfifo").arg(work.0.join("fifo.xml")).status().unwrap().success());
		cases.extend([("fifo.xml", vec![], None), ("/dev/null", vec![], None)]);
	}
	for (name, contents, position) in cases {
		if !contents.is_empty() {
			std::fs::write(work.0.join(name), contents).unwrap();
		}
		let (code, stdout, stderr) = output(work.polkameter().args(["validate", name]));
		assert_eq!(code, 2, "{name}: {stderr}");
		assert!(stdout.is_empty(), "{stdout}");
		assert!(stderr.contains(name), "{stderr}");
		if let Some((line, column)) = position {
			let at = format!("PolkaMeter: {name}: line {line}, column {column}: ");
			assert!(stderr.starts_with(&at), "{stderr}");
		}
	}
}

#[test]
fn preflight_errors_name_the_file_and_position() {
	let work = Work::new("preflight");
	work.write("core.xml", CORE_PLAN);
	let (code, stdout, stderr) = output(work.polkameter().args(["preflight", "core.xml"]));
	assert_eq!(code, 0, "{stderr}");
	assert_eq!(stdout, "Preflight passed.\n");

	let missing = CORE_PLAN.replace(
		"<setup>",
		"<plugins><plugin id=\"example\" version=\"0.1.0\" protocol=\"1\"/></plugins><setup>",
	);
	work.write("missing.xml", &missing);
	let (code, _, stderr) = output(work.polkameter().args(["preflight", "missing.xml"]));
	assert_eq!(code, 3, "{stderr}");
	let expected = "plugin example is not installed on this host; run `polkameter plugin install \
	                <path-to-executable>`";
	assert!(stderr.contains(expected), "{stderr}");
	// It names the <plugin> element and the registry it read.
	assert!(stderr.starts_with("PolkaMeter: missing.xml: line 2, "), "{stderr}");
	let registry = format!("(registry {})", work.registry().display());
	assert!(stderr.contains(&registry), "{stderr}");

	// A contract error names the file, the step, the operation and the input.
	let unknown = CORE_PLAN.replace("name=\"expected\"", "name=\"wanted\"");
	work.write("unknown.xml", &unknown);
	let (code, _, stderr) = output(work.polkameter().args(["preflight", "unknown.xml"]));
	assert_eq!(code, 3, "{stderr}");
	assert!(
		stderr.contains(
			"PolkaMeter: unknown.xml: line 4, column 91: step `check` (core.assert-equal): input \
			 `wanted` is not an input of core.assert-equal; its inputs are `actual`, `expected`"
		),
		"{stderr}"
	);
}

/// Runs `args` with standard output (or standard error) closed by its reader before the command
/// writes, and returns its exit code and the other stream.
fn with_closed(work: &Work, args: &[&str], stdout: bool) -> (i32, String) {
	let mut child = work
		.polkameter()
		.args(args)
		.stdout(Stdio::piped())
		.stderr(Stdio::piped())
		.spawn()
		.unwrap();
	if stdout {
		drop(child.stdout.take());
	} else {
		drop(child.stderr.take());
	}
	let output = child.wait_with_output().unwrap();
	let other = if stdout { output.stderr } else { output.stdout };
	(output.status.code().unwrap_or(-1), String::from_utf8(other).unwrap())
}

/// A closed or full standard output or a closed standard error never panics, keeps the exit
/// code, and the runs still write their results.
#[test]
fn broken_output_streams() {
	let work = Work::new("streams");
	work.write("core.xml", CORE_PLAN);
	let run = &["run", "core.xml", "--output", "runs"][..];
	for (args, expected) in [
		(&["validate", "core.xml"][..], 0),
		(&["validate", "missing.xml"], 2),
		(run, 0),
		(&["plugin", "list"], 0),
		(&["--help"], 0),
	] {
		let (code, stderr) = with_closed(&work, args, true);
		assert!(!stderr.contains("panicked"), "{args:?}: {stderr}");
		assert_eq!(code, expected, "{args:?}: {stderr}");
	}
	// `report` prints the load summary on standard error, through the SDK.
	smoke_run::copy_to(&work.0.join("smoke"));
	for args in [&["report", "smoke"][..], run] {
		let (code, stdout) = with_closed(&work, args, false);
		assert_eq!(code, 0, "{args:?}: {stdout}");
	}
	let mut runs = 2;
	if cfg!(target_os = "linux") {
		for args in [&["validate", "core.xml"][..], run] {
			let full = std::fs::OpenOptions::new().write(true).open("/dev/full").unwrap();
			let result = work.polkameter().args(args).stdout(full).output().unwrap();
			let stderr = String::from_utf8_lossy(&result.stderr);
			assert_eq!(result.status.code(), Some(1), "{args:?}: {stderr}");
			assert!(!stderr.contains("panicked"), "{args:?}: {stderr}");
		}
		runs += 1;
	}
	assert_eq!(std::fs::read_dir(work.0.join("runs")).unwrap().count(), runs);
}

/// A plugin executable that answers `$describe` with plugin `id`.
#[cfg(unix)]
fn script_plugin(directory: &Path, id: &str) -> PathBuf {
	let manifest = format!(r#"{{"id":"{id}","version":"0.1.0","protocol":1,"operations":{{}}}}"#);
	let path = directory.join(id);
	executable::write_executable(
		&path,
		format!(
			"#!/bin/sh\nread request\nprintf '%s\\n' '{{\"protocol\":1,\"id\":1,\"result\":{manifest}}}'\n"
		),
	);
	path
}

#[cfg(unix)]
#[test]
fn concurrent_registry_commands_keep_every_change() {
	let work = Work::new("registry");
	let children: Vec<_> = (0..12)
		.map(|i| {
			let mut command = work.polkameter();
			match i % 3 {
				0 => command
					.arg("plugin")
					.arg("install")
					.arg(script_plugin(&work.0, &format!("plugin{i}"))),
				1 => command.args([
					"plugin",
					"credential",
					&format!("profile-{i}"),
					&format!("ENV_{i}"),
				]),
				_ => command.args(["plugin", "capability", &format!("capability-{i}")]),
			};
			command.stdout(Stdio::null()).stderr(Stdio::piped()).spawn().unwrap()
		})
		.collect();
	for child in children {
		let output = child.wait_with_output().unwrap();
		assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
	}
	let registry: Value = serde_json::from_slice(&std::fs::read(work.registry()).unwrap()).unwrap();
	for i in 0..12 {
		match i % 3 {
			0 => assert_eq!(
				registry["plugins"][format!("plugin{i}")]["version"],
				"0.1.0",
				"{registry}"
			),
			1 => assert_eq!(registry["credentials"][format!("profile-{i}")], format!("ENV_{i}")),
			_ => assert!(
				registry["capabilities"]
					.as_array()
					.unwrap()
					.contains(&Value::from(format!("capability-{i}"))),
				"{registry}"
			),
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
	let preflight = |configure: &dyn Fn(&mut Command)| {
		let mut command = work.polkameter();
		command.args(["preflight", "plan.xml"]);
		configure(&mut command);
		let (code, _, stderr) = output(&mut command);
		assert_eq!(code, 3, "{stderr}");
		stderr
	};
	// Not mapped: the message names the profile, the command that maps it and the registry.
	let stderr = preflight(&|command| {
		command.env_remove(variable);
	});
	assert!(
		stderr.contains(&format!(
			"plan.xml: line 2, column 16: credential `sudo` uses profile `admin`, which is not \
			 configured on this host; run `polkameter plugin credential admin <ENV_VAR>` to name \
			 the environment variable that holds it (registry {})",
			work.registry().display()
		)),
		"{stderr}"
	);
	let (code, _, stderr) =
		output(work.polkameter().args(["plugin", "credential", "admin", variable]));
	assert_eq!(code, 0, "{stderr}");
	// `plugin list` shows the mapping, never a value.
	let (code, stdout, _) =
		output(work.polkameter().args(["plugin", "list"]).env(variable, "s3cret-value"));
	assert_eq!(code, 0);
	let listed: Value = serde_json::from_str(&stdout).unwrap();
	assert_eq!(listed["credentials"]["admin"], variable);
	assert!(!stdout.contains("s3cret-value"));
	// Mapped, but unset or empty: preflight checks it as the run would, without a step using it.
	for (value, problem) in [(None, "is not set"), (Some(""), "is empty")] {
		let stderr = preflight(&|command| {
			match value {
				Some(value) => command.env(variable, value),
				None => command.env_remove(variable),
			};
		});
		let expected = format!(
			"credential `sudo` (profile `admin`) reads environment variable {variable} (mapped by \
			 `polkameter plugin credential`), which {problem}; set {variable} on the step that \
			 runs polkameter"
		);
		assert!(stderr.contains(&expected), "{stderr}");
	}
	// --credential-env names the flag.
	let stderr = preflight(&|command| {
		command
			.args(["--credential-env", "UNSET_FLAG_VARIABLE"])
			.env_remove("UNSET_FLAG_VARIABLE");
	});
	assert!(
		stderr.contains(
			"reads environment variable UNSET_FLAG_VARIABLE (from --credential-env), which is not \
			 set; set UNSET_FLAG_VARIABLE on the step that runs polkameter, or pass the variable \
			 that holds it to --credential-env"
		),
		"{stderr}"
	);
	// A run with the credential unset fails before setup: exit 3, teardown skipped.
	let (code, _, stderr) = output(
		work.polkameter()
			.args(["run", "plan.xml", "--output", "runs"])
			.env_remove(variable),
	);
	assert_eq!(code, 3, "{stderr}");
	assert!(stderr.contains("run failed before setup:"), "{stderr}");
	let run = std::fs::read_dir(work.0.join("runs")).unwrap().next().unwrap().unwrap().path();
	let execution: Value =
		serde_json::from_slice(&std::fs::read(run.join("execution.json")).unwrap()).unwrap();
	assert_eq!(
		(execution["state"].as_str(), execution["exit_code"].as_i64()),
		(Some("failed"), Some(3))
	);
	assert!(execution.get("teardown").is_none(), "{execution}");
	assert_eq!(execution["failed_phase"], "preflight");
	// Set: both pass, and the value never reaches standard error.
	for args in [&["preflight", "plan.xml"][..], &["run", "plan.xml", "--output", "runs"]] {
		let (code, _, stderr) = output(work.polkameter().args(args).env(variable, "s3cret-value"));
		assert_eq!(code, 0, "{args:?}: {stderr}");
		assert!(!stderr.contains("s3cret-value"));
	}
}
