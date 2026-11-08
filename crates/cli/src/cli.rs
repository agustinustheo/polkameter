//! The headless CLI: validate, preflight, run and report XML plans, and manage installed plugins.

use std::{
	io::Write,
	path::{Path, PathBuf},
	sync::{
		Arc,
		atomic::{AtomicBool, Ordering},
	},
	time::Duration,
};

use crate::engine::{
	execute::{Outcome, RunOptions, RunState, Stop},
	plan::{Location, Plan, PlanError, read_plan},
	plugins::Registry,
};
use crate::shutdown::{self, Signal, Signals};
use anyhow::Context as _;
use clap::{Parser, Subcommand, ValueEnum};
use serde_json::{Value, json};

use crate::PRODUCT_NAME;

#[derive(Debug, Parser)]
#[command(
	name = "polkameter",
	version,
	about = "Headless Polkadot SDK load testing: validate, preflight, run and report XML plans",
	after_long_help = "Examples:\n  polkameter plugin install target/release/my-plugin\n  polkameter validate \
	                   plan.polkameter.xml\n  polkameter preflight plan.polkameter.xml\n  polkameter \
	                   run plan.polkameter.xml --output runs\n\nExit codes: 0 success, 1 failed run, \
	                   2 invalid input or usage, 3 preflight or setup checks failed (nothing \
	                   submitted), 130 SIGINT, 143 SIGTERM."
)]
struct Cli {
	#[command(subcommand)]
	command: Command,
}

/// Help for `--format` on every command that has it.
const FORMAT_HELP: &str = "Output format: human-readable text, or one JSON object per line on \
                           standard output, errors included";

#[derive(Debug, Subcommand)]
enum Command {
	/// Install plugins and configure this host's credentials, topologies and capabilities.
	Plugin {
		#[command(subcommand)]
		command: PluginCommand,
	},
	/// Write a starter load-test plan for the substrate plugin from a live chain.
	Init {
		/// The chain's WebSocket endpoint, such as ws://127.0.0.1:9944.
		#[arg(long)]
		rpc: String,
		/// The call to load the chain with.
		#[arg(long, value_enum, default_value_t = crate::init::Kind::Remarks)]
		kind: crate::init::Kind,
		/// The credential profile the plan names for the signers' seed.
		#[arg(long, default_value = "substrate-dev")]
		profile: String,
		/// Where to write the plan. Without it, the plan goes to standard output.
		#[arg(long)]
		output: Option<PathBuf>,
		/// Replace the --output file if it exists.
		#[arg(long)]
		force: bool,
	},
	/// Check a plan's XML, structure and built-in operations, without plugins or a chain.
	///
	/// Exits 2 and names the file, line and column of the first problem when the plan is invalid.
	#[command(
		after_long_help = "Example:\n  polkameter validate examples/plugin-data-flow.polkameter.xml"
	)]
	Validate {
		/// The plan file (`*.polkameter.xml`, at most 2 MiB), or a pipe such as /dev/stdin.
		plan: PathBuf,
		#[arg(long, value_enum, default_value_t = OutputFormat::Human, help = FORMAT_HELP)]
		format: OutputFormat,
	},
	/// Check installed plugins, required evidence and read-only steps, without submitting.
	///
	/// Starts the plan's plugins, checks every operation and input against their manifests,
	/// checks that every credential's variable is set, checks the chain and runs the <preflight>
	/// steps. Exits 3 when a check fails.
	#[command(
		after_long_help = "Example:\n  polkameter preflight examples/plugin-data-flow.polkameter.xml"
	)]
	Preflight {
		/// The plan file (`*.polkameter.xml`, at most 2 MiB), or a pipe such as /dev/stdin.
		plan: PathBuf,
		/// Environment variable for the plan's only credential, instead of its configured profile.
		#[arg(long, value_name = "ENV_VAR")]
		credential_env: Option<String>,
		#[arg(long, value_enum, default_value_t = OutputFormat::Human, help = FORMAT_HELP)]
		format: OutputFormat,
	},
	/// Run a plan.
	///
	/// Writes a run-* directory under --output with events, results, summary.md and junit.xml.
	/// Exits 3 when the run fails before its setup phase, so before it submits anything, as
	/// preflight does; teardown is then skipped.
	#[command(after_long_help = "Example:\n  polkameter run plan.polkameter.xml --output runs \
	                             --junit reports/polkameter.xml")]
	Run {
		/// The plan file (`*.polkameter.xml`, at most 2 MiB), or a pipe such as /dev/stdin.
		plan: PathBuf,
		/// The directory that receives the run-* directory.
		#[arg(long, value_name = "DIR")]
		output: PathBuf,
		/// Environment variable for the plan's only credential, instead of its configured profile.
		#[arg(long, value_name = "ENV_VAR")]
		credential_env: Option<String>,
		/// Exit 1 when a load test's verdict is FAIL. By default, verdicts do not affect the exit
		/// code; runtime and smoke-mode failures still exit 1. Plans without a load test are unaffected.
		#[arg(long)]
		fail_on_verdict: bool,
		/// Exit 1 when a load test's verdict is INCONCLUSIVE: the load ended before its rate cap,
		/// or a required check has no result. Combine with --fail-on-verdict to fail on both.
		#[arg(long)]
		fail_on_inconclusive: bool,
		/// Write an additional copy of junit.xml to this local path.
		#[arg(long, value_name = "PATH")]
		junit: Option<PathBuf>,
		/// Seconds a run gets after SIGINT or SIGTERM to finish its teardown and write its
		/// results. A second signal, or the end of this period, interrupts teardown.
		#[arg(long, value_name = "SECONDS", default_value_t = shutdown::DEFAULT_GRACE.as_secs())]
		shutdown_grace: u64,
		#[arg(long, value_enum, default_value_t = OutputFormat::Human, help = FORMAT_HELP)]
		format: OutputFormat,
	},
	/// Regenerate the checks, summary and plots of a run directory from its files.
	#[command(after_long_help = "Example:\n  polkameter report runs/run-1792255021142-12303-1")]
	Report {
		/// The run-* directory that holds execution.json and summary.md.
		artifact_directory: PathBuf,
		/// Write an additional copy of junit.xml to this path.
		#[arg(long, value_name = "PATH")]
		junit: Option<PathBuf>,
		#[arg(long, value_enum, default_value_t = OutputFormat::Human, help = FORMAT_HELP)]
		format: OutputFormat,
	},
}

#[derive(Debug, Subcommand)]
enum PluginCommand {
	/// Record a capability supplied by the local network provisioner.
	#[command(after_long_help = "Example:\n  polkameter plugin capability zombienet")]
	Capability {
		/// The capability name, as a plugin's manifest requires it.
		name: String,
	},
	/// Start a plan's plugins and print their manifests, without connecting or submitting.
	#[command(
		after_long_help = "Example:\n  polkameter plugin inspect examples/plugin-data-flow.polkameter.xml"
	)]
	Inspect {
		/// The plan file (`*.polkameter.xml`, at most 2 MiB), or a pipe such as /dev/stdin.
		#[arg(value_name = "PLAN")]
		scenario: PathBuf,
	},
	/// Register an executable after reading its manifest and pinning its checksum.
	///
	/// Run it again after every rebuild: PolkaMeter refuses to run a plugin whose file changed.
	#[command(
		after_long_help = "Example:\n  polkameter plugin install target/debug/polkameter-example-plugin"
	)]
	Install {
		/// The plugin executable; it must answer the `$describe` request.
		executable: PathBuf,
	},
	/// List installed plugins, credential profile variables, topologies and capabilities as JSON.
	///
	/// Credentials show the environment variable each profile reads, never its value.
	List,
	/// Map a credential profile to an environment variable on this host.
	///
	/// The registry stores the variable's name, never its value.
	#[command(after_long_help = "Example:\n  polkameter plugin credential sudo SUDO_SEED")]
	Credential {
		/// The profile that the plan's <credential profile="…"> names.
		profile: String,
		/// The environment variable that holds the secret when a run starts.
		#[arg(value_name = "ENV_VAR")]
		env: String,
	},
	/// Register a local Zombienet topology under a portable alias.
	#[command(after_long_help = "Example:\n  polkameter plugin topology zombie ./zombie.json")]
	Topology {
		/// The alias that the plan's <monitors topology="…"> uses.
		name: String,
		/// The Zombienet zombie.json file; stored as an absolute path.
		path: PathBuf,
	},
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
enum OutputFormat {
	/// Human-readable text.
	#[default]
	Human,
	/// One JSON object per line.
	Json,
}

/// The version of the `--format json` output: every line carries it as `schemaVersion`. It
/// changes only when a field is removed, renamed or changes meaning.
pub const OUTPUT_SCHEMA_VERSION: u32 = 1;

/// Which exit code an error maps to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ErrorKind {
	/// Exit 2: invalid input or usage.
	Invalid,
	/// Exit 3: preflight failed.
	Preflight,
	/// Exit 1: a runtime failure.
	Runtime,
}

/// A command's failure: its message and exit code, and the plan file and position it refers to,
/// when it refers to one.
#[derive(Debug)]
struct CliError {
	kind: ErrorKind,
	message: String,
	file: Option<PathBuf>,
	location: Option<Location>,
}

impl CliError {
	fn new(kind: ErrorKind, message: String) -> Self {
		Self { kind, message, file: None, location: None }
	}
	fn invalid(message: String) -> Self {
		Self::new(ErrorKind::Invalid, message)
	}
	fn preflight(message: String) -> Self {
		Self::new(ErrorKind::Preflight, message)
	}
	fn runtime(message: String) -> Self {
		Self::new(ErrorKind::Runtime, message)
	}
	/// An error about plan file `path`. The message starts with the file and, when the error has
	/// one, the line and column: `plan.xml: line 3, column 5: …`.
	fn in_plan(kind: ErrorKind, path: &Path, error: &anyhow::Error) -> Self {
		let (location, detail) = match error.downcast_ref::<PlanError>() {
			Some(located) => (located.location, located.message.clone()),
			None => (None, format!("{error:#}")),
		};
		let message = match location {
			Some(location) => format!("{}: {location}: {detail}", path.display()),
			None => format!("{}: {detail}", path.display()),
		};
		Self { kind, message, file: Some(path.to_owned()), location }
	}
	fn exit_code(&self) -> i32 {
		match self.kind {
			ErrorKind::Invalid => 2,
			ErrorKind::Preflight => 3,
			ErrorKind::Runtime => 1,
		}
	}
	/// The error as a `--format json` line for `command`.
	fn to_json(&self, command: &str) -> Value {
		let kind = match self.kind {
			ErrorKind::Invalid => "invalid",
			ErrorKind::Preflight => "preflight",
			ErrorKind::Runtime => "runtime",
		};
		let mut line = json!({
			"event": "error",
			"command": command,
			"kind": kind,
			"exitCode": self.exit_code(),
			"message": self.message,
		});
		if command == "validate" {
			line["valid"] = json!(false);
		}
		if let Some(file) = &self.file {
			// A path that is not UTF-8 is shown with replacement characters, as on standard error.
			line["file"] = json!(file.to_string_lossy());
		}
		if let Some(location) = self.location {
			line["line"] = json!(location.line);
			line["column"] = json!(location.column);
		}
		line
	}
}

impl std::fmt::Display for CliError {
	fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		formatter.write_str(&self.message)
	}
}

impl Command {
	/// The command's name if it prints `--format json`, so that an error is printed as JSON too.
	fn json_name(&self) -> Option<&'static str> {
		match self {
			Self::Validate { format: OutputFormat::Json, .. } => Some("validate"),
			Self::Preflight { format: OutputFormat::Json, .. } => Some("preflight"),
			Self::Run { format: OutputFormat::Json, .. } => Some("run"),
			Self::Report { format: OutputFormat::Json, .. } => Some("report"),
			_ => None,
		}
	}
}

pub fn main() -> i32 {
	let cli = match Cli::try_parse() {
		Ok(cli) => cli,
		Err(error) => {
			let _ = error.print();
			return error.exit_code();
		},
	};
	let json_command = cli.command.json_name();
	let result = match tokio::runtime::Runtime::new() {
		Ok(runtime) => runtime.block_on(execute(cli)),
		Err(error) => Err(CliError::runtime(format!("cannot start the async runtime: {error}"))),
	};
	let code = match result {
		Ok(code) => code,
		Err(error) => {
			eprintln!("{PRODUCT_NAME}: {error}");
			if let Some(command) = json_command {
				write_json_line(&error.to_json(command));
			}
			error.exit_code()
		},
	};
	exit_code_after_output(code, STDOUT_FAILED.load(Ordering::Relaxed))
}

async fn execute(cli: Cli) -> Result<i32, CliError> {
	match cli.command {
		Command::Plugin { command } => plugin_command(command).await,
		Command::Init { rpc, kind, profile, output, force } => {
			init(&rpc, kind, &profile, output, force).await
		},
		Command::Validate { plan: path, format } => {
			let plan = load(&path)?;
			// Built-in operations are checked here too; plugin operations need preflight.
			crate::engine::execute::validate_core_contracts(&plan)
				.map_err(|e| CliError::in_plan(ErrorKind::Invalid, &path, &e))?;
			write_result(
				format,
				&json!({"event":"validation","valid":true,"plan":plan.name}),
				"Plan is structurally valid. Preflight checks the installed plugins.".into(),
			);
			Ok(0)
		},
		Command::Preflight { plan: path, credential_env, format } => {
			let plan = load(&path)?;
			let registry = registry(&plan, credential_env)?;
			let mut result =
				crate::engine::execute::preflight(&plan, &registry).await.map_err(|e| {
					// Errors about one step or element name the plan file and position.
					if e.downcast_ref::<PlanError>().is_some_and(|e| e.location.is_some()) {
						CliError::in_plan(ErrorKind::Preflight, &path, &e)
					} else {
						CliError::preflight(format!("{e:#}"))
					}
				})?;
			result["event"] = json!("preflight");
			write_result(format, &result, "Preflight passed.".into());
			Ok(0)
		},
		Command::Run {
			plan: path,
			output,
			credential_env,
			fail_on_verdict,
			fail_on_inconclusive,
			junit,
			shutdown_grace,
			format,
		} => {
			// Before any work, so that no signal can end the process without cleanup.
			let mut signals = install_signals()?;
			let grace = Duration::from_secs(shutdown_grace);
			let plan = load(&path)?;
			let options = RunOptions { fail_on_verdict, fail_on_inconclusive };
			if output.to_str().is_none() {
				return Err(CliError::invalid(format!(
					"--output {} is not a UTF-8 path; plugins receive the run directory as text, so \
					 choose a directory whose path is UTF-8",
					output.display()
				)));
			}
			let registry = registry(&plan, credential_env)?;
			let local = LocalRun { plan, registry, output, options, junit, grace, path };
			run_local(local, format, &mut signals).await
		},
		Command::Report { artifact_directory, format, junit } => {
			report(artifact_directory, format, junit)
		},
	}
}

fn install_signals() -> Result<Signals, CliError> {
	Signals::install()
		.map_err(|e| CliError::runtime(format!("cannot install signal handlers: {e}")))
}

/// `polkameter init`: inspects the chain, renders the plan, checks that it parses and writes it.
async fn init(
	rpc: &str,
	kind: crate::init::Kind,
	profile: &str,
	output: Option<PathBuf>,
	force: bool,
) -> Result<i32, CliError> {
	if !(rpc.starts_with("ws://") || rpc.starts_with("wss://")) {
		return Err(CliError::invalid(format!("--rpc must start with ws:// or wss://, got {rpc}")));
	}
	if profile.trim().is_empty() {
		return Err(CliError::invalid("--profile must not be empty".into()));
	}
	if let Some(path) = &output {
		init_output(path, force)?;
	}
	let facts = crate::init::inspect(rpc, kind)
		.await
		.map_err(|e| CliError::preflight(e.to_string()))?;
	let xml = crate::init::render(rpc, kind, profile, &facts);
	Plan::parse(&xml)
		.map_err(|e| CliError::runtime(format!("the generated plan does not parse: {e}")))?;
	match output {
		Some(path) => {
			std::fs::write(&path, &xml)
				.map_err(|e| CliError::invalid(format!("cannot write {}: {e}", path.display())))?;
			eprintln!(
				"Wrote {} for {} ({}, {} s blocks). Next: polkameter validate {}",
				path.display(),
				facts.chain,
				facts.runtime,
				facts.block_seconds,
				path.display()
			);
		},
		None => print!("{xml}"),
	}
	Ok(0)
}

/// Checks `init --output` before `init` contacts the chain: the file must not exist unless
/// `force`, and its directory is created.
fn init_output(path: &std::path::Path, force: bool) -> Result<(), CliError> {
	if path.is_dir() {
		return Err(CliError::invalid(format!(
			"{} is a directory; pass a file such as {}",
			path.display(),
			path.join("load.polkameter.xml").display()
		)));
	}
	if path.exists() && !force {
		return Err(CliError::invalid(format!(
			"{} exists; pass --force to replace it",
			path.display()
		)));
	}
	if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
		std::fs::create_dir_all(parent)
			.map_err(|e| CliError::invalid(format!("cannot create {}: {e}", parent.display())))?;
	}
	Ok(())
}

/// The parsed plan.
fn load(path: &Path) -> Result<Plan, CliError> {
	let xml = read_plan(path).map_err(|e| {
		// Text that is not UTF-8 has a position; the other read errors already name the file.
		if e.downcast_ref::<PlanError>().is_some() {
			CliError::in_plan(ErrorKind::Invalid, path, &e)
		} else {
			CliError { file: Some(path.to_owned()), ..CliError::invalid(format!("{e:#}")) }
		}
	})?;
	Plan::parse(&xml).map_err(|e| CliError::in_plan(ErrorKind::Invalid, path, &e))
}

/// This host's registry; `credential_env` replaces the profile of the plan's only credential.
fn registry(plan: &Plan, credential_env: Option<String>) -> Result<Registry, CliError> {
	let mut registry = Registry::load().map_err(|e| CliError::invalid(format!("{e:#}")))?;
	if let Some(variable) = credential_env {
		let [credential] = plan.credentials.entries.as_slice() else {
			return Err(CliError::invalid(format!(
				"--credential-env needs a plan with exactly one <credential>, but this plan has \
				 {}; map each profile with `polkameter plugin credential <profile> <ENV_VAR>` \
				 instead",
				plan.credentials.entries.len()
			)));
		};
		registry.credentials.insert(credential.profile.clone(), variable);
		registry.flag_profile = Some(credential.profile.clone());
	}
	Ok(registry)
}

/// A local run and where its results go.
struct LocalRun {
	/// The plan file, for messages that point into it.
	path: PathBuf,
	plan: Plan,
	registry: Registry,
	output: PathBuf,
	options: RunOptions,
	junit: Option<PathBuf>,
	grace: Duration,
}

async fn run_local(
	run: LocalRun,
	format: OutputFormat,
	signals: &mut Signals,
) -> Result<i32, CliError> {
	let LocalRun { path, plan, registry, output, options, junit, grace } = run;
	let stop = Stop::default();
	let sink = Arc::new(move |event: Value| {
		if format == OutputFormat::Json {
			write_json_line(&event);
		} else if event["event"] == "phase" {
			eprintln!("Phase: {}", event["phase"].as_str().unwrap_or("unknown"));
		}
	});
	let secrets = available_secrets(&registry);
	let work =
		crate::engine::execute::run_stoppable(plan, registry, &output, stop.clone(), sink, options);
	let supervised = shutdown::supervise(
		work,
		signals,
		grace,
		|signal| {
			eprintln!(
				"{PRODUCT_NAME}: received {}; stopping the run (teardown has up to {} s; send the signal again to stop now)",
				signal.name(),
				grace.as_secs()
			);
			stop.request(signal.exit_code());
		},
		|signal| {
			eprintln!("{PRODUCT_NAME}: stopping now; teardown is interrupted");
			stop.force(signal.exit_code());
		},
	)
	.await;
	let Some(result) = supervised.result else {
		let signal = supervised.signal.unwrap_or(Signal::Interrupt);
		eprintln!(
			"{PRODUCT_NAME}: the run did not write its outcome within {} s of a forced stop",
			shutdown::FORCE_LIMIT.as_secs()
		);
		return Ok(signal.exit_code());
	};
	let outcome = result.map_err(|e| CliError::runtime(e.to_string()))?;
	if let Err(error) = copy_junit(&outcome.artifact_dir, junit.as_deref()) {
		junit_warning(format, Some(&outcome.artifact_dir), &error.to_string(), &secrets);
	}
	write_artifact_written(format, &path, &outcome);
	Ok(outcome.exit_code)
}

/// Reports a finished run's artifacts. A failed run, or one that completed with failures, also
/// prints its reason to standard error in both formats. When a failed run's reason points into
/// the plan, the JSON line also has the plan's `file`, `line` and `column`, as an `error` line
/// does.
fn write_artifact_written(format: OutputFormat, plan: &Path, outcome: &Outcome) {
	let mut line = json!({"event":"artifact-written","outcome":outcome});
	if let Some(at) = outcome.error_location.filter(|_| outcome.state == RunState::Failed) {
		line["file"] = json!(plan.to_string_lossy());
		line["line"] = json!(at.line);
		line["column"] = json!(at.column);
	}
	write_result(
		format,
		&line,
		format!("Run {}. Artifacts: {}", outcome.state, outcome.artifact_dir.display()),
	);
	if let Some(reason) = failure_reason(plan, outcome) {
		eprintln!("{PRODUCT_NAME}: {reason}");
	}
}

/// Longest reason, in characters, that the failure line prints. `execution.json` keeps it all.
const MAX_REASON_CHARS: usize = 1_000;

/// The one-line reason for a failed run, naming the file that holds the details; for a run that
/// completed with failures, its recorded exit reason (smoke problems, or the verdict and flag).
/// A reason that points into the plan starts with the plan file, as plan errors do:
/// `plan.xml: line 4, …`. A run that failed before setup says so: it exits 3 and submitted
/// nothing.
fn failure_reason(plan: &Path, outcome: &Outcome) -> Option<String> {
	if outcome.state == RunState::CompletedWithFailures {
		return Some(one_line(outcome.exit_reason.as_deref()?));
	}
	if outcome.state != RunState::Failed {
		return None;
	}
	let mut error = outcome.error.clone().unwrap_or_else(|| "no reason recorded".into());
	if outcome.error_location.is_some() {
		error = format!("{}: {error}", plan.display());
	}
	let error = one_line(&error);
	let failed = if outcome.exit_code == crate::engine::execute::PREFLIGHT_EXIT_CODE {
		"failed before setup"
	} else {
		"failed"
	};
	let execution = outcome.artifact_dir.join("execution.json");
	Some(format!("run {failed}: {error} (details in {})", execution.display()))
}

/// `text` made safe for one terminal line. CSI escape sequences (colours, cursor movement) are
/// removed, line breaks become ` ⏎ `, and other unsafe characters (see
/// [`crate::redact::is_unsafe_char`]), including the ESC of any other escape sequence, are escaped
/// as `\u{…}`. The rendered line is cut to at most [`MAX_REASON_CHARS`] characters, never inside
/// an escape, followed by `…`.
fn one_line(text: &str) -> String {
	let mut line = String::new();
	let mut length = 0;
	let mut chars = text.trim().chars().peekable();
	while let Some(c) = chars.next() {
		let piece = match c {
			// CSI sequence: ESC [ parameters final-byte.
			'\u{1b}' if chars.peek() == Some(&'[') => {
				chars.next();
				for c in chars.by_ref() {
					if ('\u{40}'..='\u{7e}').contains(&c) {
						break;
					}
				}
				continue;
			},
			'\r' | '\n' => {
				if c == '\r' && chars.peek() == Some(&'\n') {
					chars.next();
				}
				" ⏎ ".to_owned()
			},
			c if crate::redact::is_unsafe_char(c) => c.escape_unicode().to_string(),
			c => c.to_string(),
		};
		let piece_length = piece.chars().count();
		if length + piece_length > MAX_REASON_CHARS {
			line.push('…');
			break;
		}
		line.push_str(&piece);
		length += piece_length;
	}
	line
}

/// Refuses a `summary.json` this version cannot rewrite: one without a `schemaVersion`, or with
/// a version other than the one it writes. A run without a summary (no load test) passes.
fn check_summary_version(directory: &std::path::Path) -> Result<(), CliError> {
	use polkameter_plugin_sdk::observers::files::summary::SCHEMA_VERSION;
	let path = directory.join("summary.json");
	let bytes = match std::fs::read(&path) {
		Ok(bytes) => bytes,
		Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
		Err(e) => return Err(CliError::runtime(format!("cannot read {}: {e}", path.display()))),
	};
	let summary: Value = serde_json::from_slice(&bytes)
		.map_err(|e| CliError::invalid(format!("{} is not valid JSON: {e}", path.display())))?;
	let unsupported = |found: String| {
		CliError::invalid(format!(
			"{} has schemaVersion {found}, but polkameter {} reads and writes only schemaVersion \
			 {SCHEMA_VERSION}; report it with the polkameter version that recorded the run. \
			 Nothing was rewritten",
			path.display(),
			env!("CARGO_PKG_VERSION"),
		))
	};
	match summary.get("schemaVersion") {
		Some(Value::Number(n)) if n.as_u64() == Some(u64::from(SCHEMA_VERSION)) => Ok(()),
		Some(other) => Err(unsupported(other.to_string())),
		None => Err(unsupported("missing".into())),
	}
}

fn report(path: PathBuf, format: OutputFormat, junit: Option<PathBuf>) -> Result<i32, CliError> {
	let runtime = |e: anyhow::Error| CliError::runtime(format!("{e:#}"));
	check_summary_version(&path)?;
	crate::engine::artifacts::write_outputs(&path)
		.with_context(|| format!("cannot read run directory {}", path.display()))
		.map_err(runtime)?;
	// The credentials this host has now. The run's endpoints are not known here: the files
	// hold them redacted already.
	let secrets = Registry::load()
		.map(|registry| available_secrets(&registry))
		.unwrap_or_default();
	if path.join("summary.json").is_file() {
		let redactor = crate::redact::Redactor::new(&secrets, None::<&str>);
		crate::engine::measurement::check(&path, &redactor).map_err(runtime)?;
	}
	let redactor = crate::redact::Redactor::new(&secrets, None::<&str>);
	if let Err(error) = crate::engine::junit::write(&path, &redactor) {
		junit_warning(format, Some(&path), &error.to_string(), &secrets);
	} else if let Err(error) = copy_junit(&path, junit.as_deref()) {
		junit_warning(format, Some(&path), &error.to_string(), &secrets);
	}
	let execution = path.join("execution.json");
	let outcome: crate::engine::execute::Outcome = serde_json::from_slice(
		&std::fs::read(&execution)
			.map_err(|e| CliError::runtime(format!("cannot read {}: {e}", execution.display())))?,
	)
	.map_err(|e| CliError::runtime(e.to_string()))?;
	let summary_path = path.join("summary.md");
	let summary = std::fs::read_to_string(&summary_path)
		.map_err(|e| CliError::runtime(format!("cannot read {}: {e}", summary_path.display())))?;
	write_result(format, &json!({"event":"report","outcome":outcome,"summary":summary}), summary);
	Ok(0)
}

fn available_secrets(registry: &Registry) -> Vec<String> {
	registry
		.credentials
		.values()
		.filter_map(|name| std::env::var(name).ok())
		.filter(|value| !value.is_empty())
		.collect()
}

fn junit_warning(
	format: OutputFormat,
	directory: Option<&std::path::Path>,
	message: &str,
	secrets: &[String],
) {
	let event = crate::engine::junit::warning(directory, message, secrets);
	if format == OutputFormat::Json {
		write_json_line(&event);
	}
}

/// Keep the default report even when the caller requests a copy (including the same path).
fn copy_junit(
	directory: &std::path::Path,
	destination: Option<&std::path::Path>,
) -> Result<(), CliError> {
	if let Some(destination) = destination {
		let xml = std::fs::read_to_string(directory.join("junit.xml"))
			.map_err(|e| CliError::runtime(format!("cannot read junit.xml: {e}")))?;
		write_junit_copy(destination, &xml)?;
	}
	Ok(())
}

fn write_junit_copy(destination: &std::path::Path, xml: &str) -> Result<(), CliError> {
	let result = (|| -> std::io::Result<()> {
		if let Some(parent) = destination.parent().filter(|p| !p.as_os_str().is_empty()) {
			std::fs::create_dir_all(parent)?;
		}
		std::fs::write(destination, xml)
	})();
	result.map_err(|e| {
		CliError::runtime(format!("cannot write JUnit copy {}: {e}", destination.display()))
	})
}

/// A JSON line in JSON mode, else the human text on stdout.
fn write_result(format: OutputFormat, json_value: &Value, human: String) {
	if format == OutputFormat::Json {
		write_json_line(json_value);
	} else {
		write_stdout(&human);
	}
}

/// Writes one JSON line, adding `schemaVersion` to an object that lacks it.
fn write_json_line(value: &Value) {
	let mut value = value.clone();
	if let Some(object) = value.as_object_mut() {
		object.entry("schemaVersion").or_insert(json!(OUTPUT_SCHEMA_VERSION));
	}
	write_stdout(&value.to_string());
}

/// Set once standard output is gone, for example after `| head -1` exits.
static STDOUT_CLOSED: AtomicBool = AtomicBool::new(false);
/// Set once a write to standard output failed for another reason, such as a full disk.
static STDOUT_FAILED: AtomicBool = AtomicBool::new(false);

/// Writes `text` and a newline to standard output. When the reader has closed the pipe, output
/// stops silently and the command carries on: a run still finishes, writes its results and
/// exits with its own code. Any other failure, such as a full disk, is reported once on standard
/// error; output stops, the command carries on, and it exits 1 instead of 0 (see
/// [`exit_code_after_output`]).
fn write_stdout(text: &str) {
	if STDOUT_CLOSED.load(Ordering::Relaxed) || STDOUT_FAILED.load(Ordering::Relaxed) {
		return;
	}
	let mut stdout = std::io::stdout().lock();
	let written = write_line(&mut stdout, text);
	match written.map_err(|error| classify_stdout_error(&error)) {
		Ok(()) => {},
		Err(StdoutError::Closed) => STDOUT_CLOSED.store(true, Ordering::Relaxed),
		Err(StdoutError::Failed(error)) => {
			if !STDOUT_FAILED.swap(true, Ordering::Relaxed) {
				eprintln!("{PRODUCT_NAME}: cannot write to standard output: {error}");
			}
		},
	}
}

fn write_line(out: &mut impl Write, text: &str) -> std::io::Result<()> {
	out.write_all(text.as_bytes())?;
	out.write_all(b"\n")?;
	out.flush()
}

/// Why a write to standard output failed.
#[derive(Debug, Eq, PartialEq)]
enum StdoutError {
	/// The reader went away (`| head -1`): not an error.
	Closed,
	/// Anything else, such as a full disk or a file over its size limit.
	Failed(String),
}

fn classify_stdout_error(error: &std::io::Error) -> StdoutError {
	if error.kind() == std::io::ErrorKind::BrokenPipe {
		StdoutError::Closed
	} else {
		StdoutError::Failed(error.to_string())
	}
}

/// The process's exit code: the command's, or 1 when it succeeded but its output could not be
/// written.
fn exit_code_after_output(code: i32, output_failed: bool) -> i32 {
	if output_failed && code == 0 { 1 } else { code }
}

async fn plugin_command(command: PluginCommand) -> Result<i32, CliError> {
	let invalid = |e: anyhow::Error| CliError::invalid(format!("{e:#}"));
	let path = Registry::path().map_err(invalid)?;
	let result = match command {
		PluginCommand::List => {
			let registry = Registry::read(&path).map_err(invalid)?;
			// Credentials are listed by variable name; no value is read.
			json!({
				"plugins": registry.plugins,
				"credentials": registry.credentials,
				"topologies": registry.topologies.iter()
					.map(|(alias, file)| (alias.clone(), file.to_string_lossy().into_owned()))
					.collect::<std::collections::BTreeMap<_, _>>(),
				"capabilities": registry.capabilities,
			})
		},
		PluginCommand::Inspect { scenario } => {
			let plan = load(&scenario)?;
			let registry = Registry::read(&path).map_err(invalid)?;
			crate::engine::execute::inspect(&plan, &registry).await.map_err(|e| {
				if e.downcast_ref::<PlanError>().is_some_and(|e| e.location.is_some()) {
					CliError::in_plan(ErrorKind::Invalid, &scenario, &e)
				} else {
					invalid(e)
				}
			})?
		},
		PluginCommand::Install { executable } => {
			// Ask the plugin for its manifest before taking the lock: it may take seconds.
			let (manifest, installation) =
				Registry::describe(&executable).await.map_err(invalid)?;
			let location = installation.executable.display().to_string();
			update_registry(&path, |registry| {
				registry.plugins.insert(manifest.id.clone(), installation);
			})?;
			let count = manifest.operations.len();
			let noun = if count == 1 { "operation" } else { "operations" };
			write_stdout(&format!(
				"Installed plugin {} {} ({count} {noun}) from {location}",
				manifest.id, manifest.version
			));
			return Ok(0);
		},
		PluginCommand::Capability { name } => {
			update_registry(&path, |registry| {
				if !registry.capabilities.contains(&name) {
					registry.capabilities.push(name);
				}
			})?;
			json!({"configured":true})
		},
		PluginCommand::Credential { profile, env } => {
			update_registry(&path, |registry| {
				registry.credentials.insert(profile, env);
			})?;
			json!({"configured":true})
		},
		PluginCommand::Topology { name, path: topology } => {
			let topology = topology.canonicalize().map_err(|e| {
				CliError::invalid(format!("cannot read topology {}: {e}", topology.display()))
			})?;
			update_registry(&path, |registry| {
				registry.topologies.insert(name, topology);
			})?;
			json!({"configured":true})
		},
	};
	let pretty =
		serde_json::to_string_pretty(&result).map_err(|e| CliError::runtime(e.to_string()))?;
	write_stdout(&pretty);
	Ok(0)
}

/// Applies `change` to the registry at `path` under its lock, so that concurrent `polkameter
/// plugin` commands do not lose each other's changes, and replaces the file atomically.
fn update_registry(path: &Path, change: impl FnOnce(&mut Registry)) -> Result<(), CliError> {
	let invalid = |e: anyhow::Error| CliError::invalid(format!("{e:#}"));
	// Through a symbolic link: the lock, the temporary file and the write go to its target.
	let path = &Registry::resolve(path).map_err(invalid)?;
	let _lock = Registry::lock(path).map_err(invalid)?;
	let mut registry = Registry::read(path).map_err(invalid)?;
	change(&mut registry);
	registry.write(path).map_err(invalid)
}
