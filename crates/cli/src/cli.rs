//! The headless CLI: validate, preflight, run and report XML plans, manage installed plugins,
//! and serve or use a remote agent.

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

use crate::{
	PRODUCT_NAME,
	remote::{self, RemoteRunnerTarget},
};

#[derive(Debug, Parser)]
#[command(
	name = "polkameter",
	version,
	about = "Headless Polkadot SDK load testing: validate, preflight, run and report XML plans",
	after_long_help = "Examples:\n  polkameter plugin install target/release/my-plugin\n  polkameter validate \
	                   plan.polkameter.xml\n  polkameter preflight plan.polkameter.xml\n  polkameter \
	                   run plan.polkameter.xml --output runs\n\nExit codes: 0 success, 1 failed run, \
	                   2 invalid input or usage, 3 preflight failed, 130 SIGINT, 143 SIGTERM."
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
	/// Check a plan's XML and structure, without plugins or a chain.
	///
	/// Exits 2 and names the file, line and column of the first problem when the plan is invalid.
	#[command(
		after_long_help = "Example:\n  polkameter validate examples/plugin-workflow.polkameter.xml"
	)]
	Validate {
		/// The plan file (`*.polkameter.xml`, at most 2 MiB).
		plan: PathBuf,
		#[arg(long, value_enum, default_value_t = OutputFormat::Human, help = FORMAT_HELP)]
		format: OutputFormat,
	},
	/// Check installed plugins, required evidence and read-only steps, without submitting.
	///
	/// Starts the plan's plugins, checks every operation and input against their manifests,
	/// checks the chain and runs the <preflight> steps. Exits 3 when a check fails.
	#[command(
		after_long_help = "Example:\n  polkameter preflight examples/plugin-workflow.polkameter.xml"
	)]
	Preflight {
		/// The plan file (`*.polkameter.xml`, at most 2 MiB).
		plan: PathBuf,
		/// Environment variable for the plan's only credential, instead of its configured profile.
		#[arg(long, value_name = "ENV_VAR")]
		credential_env: Option<String>,
		#[arg(long, value_enum, default_value_t = OutputFormat::Human, help = FORMAT_HELP)]
		format: OutputFormat,
	},
	/// Run a plan locally or through an authenticated remote agent.
	///
	/// A local run writes a run-* directory under --output with events, results, summary.md and
	/// junit.xml. A remote run sends the plan to an agent, which keeps the run directory.
	#[command(after_long_help = "Examples:\n  polkameter run plan.polkameter.xml --output runs \
	                             --junit reports/polkameter.xml\n  polkameter run \
	                             plan.polkameter.xml --remote https://agent.example:9901 \
	                             --remote-token-env POLKAMETER_AGENT_TOKEN")]
	Run {
		/// The plan file (`*.polkameter.xml`, at most 2 MiB).
		plan: PathBuf,
		/// Local artifact root. Required unless --remote is used because remote agents own their
		/// artifact root.
		#[arg(long, value_name = "DIR", required_unless_present = "remote")]
		output: Option<PathBuf>,
		/// Environment variable for the plan's only credential, instead of its configured profile.
		#[arg(long, value_name = "ENV_VAR", conflicts_with = "remote")]
		credential_env: Option<String>,
		/// Exit 1 when a load test's verdict is FAIL. By default, verdicts do not affect the exit
		/// code; runtime and smoke-mode failures still exit 1. Plans without a load test are unaffected.
		#[arg(long)]
		fail_on_verdict: bool,
		/// Write an additional copy of junit.xml to this local path.
		#[arg(long, value_name = "PATH")]
		junit: Option<PathBuf>,
		/// Remote runner endpoint. It must use HTTPS or be a loopback HTTP tunnel.
		#[arg(long, value_name = "URL")]
		remote: Option<String>,
		/// Environment variable holding the remote-agent bearer token.
		#[arg(long, value_name = "ENV_VAR", requires = "remote")]
		remote_token_env: Option<String>,
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
	/// Start an authenticated remote runner agent.
	Agent {
		#[command(subcommand)]
		command: AgentCommand,
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
		after_long_help = "Example:\n  polkameter plugin inspect examples/plugin-workflow.polkameter.xml"
	)]
	Inspect {
		/// The plan file (`*.polkameter.xml`, at most 2 MiB).
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
	/// List installed plugins as JSON.
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

#[derive(Debug, Subcommand)]
enum AgentCommand {
	/// Serve remote run requests through the versioned PolkaMeter agent protocol.
	///
	/// The agent serves plain HTTP on a loopback address; reach it through an SSH tunnel or a TLS
	/// proxy.
	#[command(after_long_help = "Example:\n  POLKAMETER_AGENT_TOKEN=$(openssl rand -hex 32) \
	                             polkameter agent serve --bind 127.0.0.1:9901")]
	Serve {
		/// Loopback address and port to listen on.
		#[arg(long, value_name = "ADDR", default_value = "127.0.0.1:9901")]
		bind: String,
		/// Environment variable that holds the bearer token (at least 32 characters).
		#[arg(long, value_name = "ENV_VAR", default_value = "POLKAMETER_AGENT_TOKEN")]
		token_env: String,
		/// Directory for run directories; relative paths start from the working directory.
		#[arg(long, value_name = "DIR", default_value = "target/polkameter-agent-runs")]
		output_root: String,
		/// Keep only the newest N run directories under --output-root, deleting older ones
		/// when a run starts. By default every run directory is kept.
		#[arg(long, value_name = "N", value_parser = clap::value_parser!(u64).range(1..))]
		keep_runs: Option<u64>,
		/// Seconds the active run gets after SIGINT or SIGTERM to finish its teardown. A second
		/// signal, or the end of this period, interrupts teardown.
		#[arg(long, value_name = "SECONDS", default_value_t = shutdown::DEFAULT_GRACE.as_secs())]
		shutdown_grace: u64,
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
			line["file"] = json!(file);
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
	match result {
		Ok(code) => code,
		Err(error) => {
			eprintln!("{PRODUCT_NAME}: {error}");
			if let Some(command) = json_command {
				write_json_line(&error.to_json(command));
			}
			error.exit_code()
		},
	}
}

async fn execute(cli: Cli) -> Result<i32, CliError> {
	match cli.command {
		Command::Plugin { command } => plugin_command(command).await,
		Command::Validate { plan, format } => {
			let (_, plan) = load(&plan)?;
			write_result(
				format,
				&json!({"event":"validation","valid":true,"plan":plan.name}),
				"Plan is structurally valid. Preflight checks the installed plugins.".into(),
			);
			Ok(0)
		},
		Command::Preflight { plan: path, credential_env, format } => {
			let (_, plan) = load(&path)?;
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
			remote,
			remote_token_env,
			fail_on_verdict,
			junit,
			shutdown_grace,
			format,
		} => {
			// Before any work, so that no signal can end the process without cleanup.
			let mut signals = install_signals()?;
			let grace = Duration::from_secs(shutdown_grace);
			let (xml, plan) = load(&path)?;
			let options = RunOptions { fail_on_verdict };
			match remote {
				Some(endpoint) => {
					let limits = remote::Follow::for_plan(
						Duration::from_millis(plan.timeout_ms),
						teardown_budget(&plan),
						grace,
					);
					let target = remote_target(endpoint, remote_token_env)?;
					let remote = RemoteRun { target, xml, options, junit, limits };
					run_plugin_remote(remote, format, &mut signals).await
				},
				None => {
					let output = output.ok_or_else(|| {
						CliError::invalid("--output is required without --remote".into())
					})?;
					let registry = registry(&plan, credential_env)?;
					let local = LocalRun { plan, registry, output, options, junit, grace };
					run_local(local, format, &mut signals).await
				},
			}
		},
		Command::Report { artifact_directory, format, junit } => {
			report(artifact_directory, format, junit)
		},
		Command::Agent { command } => agent(command).await,
	}
}

fn install_signals() -> Result<Signals, CliError> {
	Signals::install()
		.map_err(|e| CliError::runtime(format!("cannot install signal handlers: {e}")))
}

/// The longest a plan's teardown can take: each step's `timeout-ms`, plus a plugin restart
/// (`$describe`, up to 10 s) for each step.
fn teardown_budget(plan: &Plan) -> Duration {
	plan.teardown
		.entries
		.iter()
		.map(|step| Duration::from_millis(step.timeout_ms) + Duration::from_secs(10))
		.sum()
}

/// The plan's XML, which remote runs send as-is, and its parsed form.
fn load(path: &Path) -> Result<(String, Plan), CliError> {
	let xml = read_plan(path).map_err(|e| CliError {
		file: Some(path.to_owned()),
		..CliError::invalid(format!("{e:#}"))
	})?;
	let plan = Plan::parse(&xml).map_err(|e| CliError::in_plan(ErrorKind::Invalid, path, &e))?;
	Ok((xml, plan))
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
	}
	Ok(registry)
}

/// A local run and where its results go.
struct LocalRun {
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
	let LocalRun { plan, registry, output, options, junit, grace } = run;
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
		None,
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
	write_artifact_written(format, RunLocation::Local, &outcome);
	Ok(outcome.exit_code)
}

/// Where a run executed, which decides how its result lines are worded.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RunLocation {
	Local,
	Remote,
}

/// Reports a finished run's artifacts. A failed run also prints its reason to standard error in
/// both formats.
fn write_artifact_written(format: OutputFormat, location: RunLocation, outcome: &Outcome) {
	let label = match location {
		RunLocation::Local => "Run",
		RunLocation::Remote => "Remote run",
	};
	write_result(
		format,
		&json!({"event":"artifact-written","outcome":outcome}),
		format!("{label} {}. Artifacts: {}", outcome.state, outcome.artifact_dir.display()),
	);
	if let Some(reason) = failure_reason(location, outcome) {
		eprintln!("{PRODUCT_NAME}: {reason}");
	}
}

/// Longest reason, in characters, that the failure line prints. `execution.json` keeps it all.
const MAX_REASON_CHARS: usize = 300;

/// The one-line reason for a failed run, naming the file that holds the details.
fn failure_reason(location: RunLocation, outcome: &Outcome) -> Option<String> {
	if outcome.state != RunState::Failed {
		return None;
	}
	let error = one_line(outcome.error.as_deref().unwrap_or("no reason recorded"));
	let execution = outcome.artifact_dir.join("execution.json");
	Some(match location {
		RunLocation::Local => {
			format!("run failed: {error} (details in {})", execution.display())
		},
		RunLocation::Remote => {
			format!("remote run failed: {error} (details in {} on the agent)", execution.display())
		},
	})
}

/// `text` made safe for one terminal line. CSI escape sequences (colours, cursor movement) are
/// removed, line breaks become ` ⏎ `, and other control characters, including the ESC of any other
/// escape sequence, and bidirectional-override characters are escaped as `\u{…}`. The rendered
/// line is cut to at most [`MAX_REASON_CHARS`] characters, never inside an escape, followed by `…`.
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
			c if c.is_control()
				|| matches!(c, '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}') =>
			{
				c.escape_unicode().to_string()
			},
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
	if path.join("summary.json").is_file() {
		crate::engine::measurement::check(&path).map_err(runtime)?;
	}
	let secrets = Registry::load()
		.map(|registry| available_secrets(&registry))
		.unwrap_or_default();
	if let Err(error) = crate::engine::junit::write(&path, &secrets) {
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

async fn agent(command: AgentCommand) -> Result<i32, CliError> {
	match command {
		AgentCommand::Serve { bind, token_env, output_root, keep_runs, shutdown_grace } => {
			let mut signals = install_signals()?;
			let token = std::env::var(&token_env).map_err(|_| {
				CliError::invalid(format!(
					"agent token environment variable {token_env} is not set"
				))
			})?;
			remote::check_token(&token).map_err(|error| {
				CliError::invalid(format!("agent token in {token_env} {error}"))
			})?;
			let settings = remote::AgentSettings {
				output_root,
				keep_runs: keep_runs.map(|keep| usize::try_from(keep).unwrap_or(usize::MAX)),
				grace: Duration::from_secs(shutdown_grace),
			};
			let signal = remote::serve(&bind, token, settings, &mut signals)
				.await
				.map_err(CliError::runtime)?;
			Ok(signal.exit_code())
		},
	}
}

fn remote_target(
	endpoint: String,
	token_env: Option<String>,
) -> Result<RemoteRunnerTarget, CliError> {
	let token_env = token_env
		.ok_or_else(|| CliError::invalid("--remote-token-env is required with --remote".into()))?;
	let bearer_token = std::env::var(&token_env).map_err(|_| {
		CliError::invalid(format!("remote token environment variable {token_env} is not set"))
	})?;
	let target = RemoteRunnerTarget { endpoint, bearer_token };
	target.validate().map_err(CliError::invalid)?;
	Ok(target)
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

/// Writes `text` and a newline to standard output. When the reader has closed the pipe, output
/// stops silently and the command carries on: a run still finishes, writes its results and
/// exits with its own code.
fn write_stdout(text: &str) {
	if STDOUT_CLOSED.load(Ordering::Relaxed) {
		return;
	}
	let mut stdout = std::io::stdout().lock();
	let written = stdout
		.write_all(text.as_bytes())
		.and_then(|()| stdout.write_all(b"\n"))
		.and_then(|()| stdout.flush());
	if written.is_err() {
		STDOUT_CLOSED.store(true, Ordering::Relaxed);
	}
}

async fn plugin_command(command: PluginCommand) -> Result<i32, CliError> {
	let invalid = |e: anyhow::Error| CliError::invalid(format!("{e:#}"));
	let path = Registry::path().map_err(invalid)?;
	let result = match command {
		PluginCommand::List => {
			let registry = Registry::read(&path).map_err(invalid)?;
			serde_json::to_value(&registry.plugins).map_err(|e| invalid(e.into()))?
		},
		PluginCommand::Inspect { scenario } => {
			let (_, plan) = load(&scenario)?;
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
	let _lock = Registry::lock(path).map_err(invalid)?;
	let mut registry = Registry::read(path).map_err(invalid)?;
	change(&mut registry);
	registry.write(path).map_err(invalid)
}

/// A remote run: the agent, the plan, and where a JUnit copy goes.
struct RemoteRun {
	target: RemoteRunnerTarget,
	xml: String,
	options: RunOptions,
	junit: Option<PathBuf>,
	limits: remote::Follow,
}

async fn run_plugin_remote(
	run: RemoteRun,
	format: OutputFormat,
	signals: &mut Signals,
) -> Result<i32, CliError> {
	let RemoteRun { target, xml, options, junit, limits } = run;
	let followed = remote::follow_run(&target, xml, options, signals, &limits)
		.await
		.map_err(CliError::runtime)?;
	let (status, signal) = match followed {
		remote::Followed::Ended { status, signal } => (status, signal),
		remote::Followed::Abandoned { id, signal } => {
			eprintln!(
				"{PRODUCT_NAME}: stopped waiting for remote run {id}; the agent keeps its run directory"
			);
			return Ok(signal.exit_code());
		},
	};
	let Some(mut outcome) = status.outcome else {
		return Err(CliError::runtime(
			status.error.unwrap_or_else(|| "remote run produced no outcome".into()),
		));
	};
	if let Some(path) = junit.as_deref() {
		let result = match remote::plugin_junit(&status.id, &target).await {
			Ok(xml) => write_junit_copy(path, &xml),
			Err(error) => Err(CliError::runtime(format!("cannot fetch junit.xml: {error}"))),
		};
		if let Err(error) = result {
			junit_warning(
				format,
				None,
				&error.to_string(),
				std::slice::from_ref(&target.bearer_token),
			);
		}
	}
	// The agent records 130 for any stop; this process reports the signal it received.
	if let (Some(signal), RunState::Stopped) = (signal, outcome.state) {
		outcome.exit_code = signal.exit_code();
	}
	write_artifact_written(format, RunLocation::Remote, &outcome);
	Ok(outcome.exit_code)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn offline_report_reproduces_retained_smoke_verdicts() {
		let fixture =
			std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/smoke-run");
		let directory = std::env::temp_dir().join(format!(
			"polkameter-report-replay-{}-{}",
			std::process::id(),
			std::time::SystemTime::now()
				.duration_since(std::time::UNIX_EPOCH)
				.unwrap()
				.as_nanos()
		));
		std::fs::create_dir(&directory).unwrap();
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
		let recorded: Value =
			serde_json::from_slice(&std::fs::read(directory.join("summary.json")).unwrap())
				.unwrap();
		// The CLI report path also invokes measurement::check from the raw files.
		assert_eq!(report(directory.clone(), OutputFormat::Json, None).unwrap(), 0);
		let replayed: Value =
			serde_json::from_slice(&std::fs::read(directory.join("summary.json")).unwrap())
				.unwrap();
		let verdicts = |summary: &Value| {
			summary["checks"]
				.as_array()
				.unwrap()
				.iter()
				.map(|check| (check["check"].clone(), check["status"].clone()))
				.collect::<Vec<_>>()
		};
		assert_eq!(verdicts(&replayed), verdicts(&recorded));
		assert_eq!(replayed["verdict"]["result"], "FAIL");
		assert!(
			std::fs::read_to_string(directory.join("summary.md"))
				.unwrap()
				.contains("- **Verdict:** FAIL")
		);
		assert_eq!(replayed["loss"], recorded["loss"]);
		std::fs::remove_dir_all(directory).unwrap();
	}

	#[test]
	fn missing_plan_error_names_the_file_and_keeps_the_cause() {
		let Err(error) = load(std::path::Path::new("does/not/exist.xml")) else {
			panic!("a missing plan loaded");
		};
		let cause = std::fs::read_to_string("does/not/exist.xml").unwrap_err();
		assert_eq!(error.to_string(), format!("cannot read plan does/not/exist.xml: {cause}"));
	}

	#[test]
	fn only_failed_runs_have_a_failure_reason() {
		let outcome = |state, error: Option<&str>| Outcome {
			artifact_dir: PathBuf::from("runs/run-1"),
			exit_code: 1,
			state,
			error: error.map(Into::into),
			teardown: None,
		};
		let execution = std::path::Path::new("runs/run-1").join("execution.json");
		let local = |state, error| failure_reason(RunLocation::Local, &outcome(state, error));
		assert_eq!(
			local(RunState::Failed, Some("assertion check failed")),
			Some(format!(
				"run failed: assertion check failed (details in {})",
				execution.display()
			))
		);
		assert_eq!(
			failure_reason(RunLocation::Remote, &outcome(RunState::Failed, Some("boom"))),
			Some(format!(
				"remote run failed: boom (details in {} on the agent)",
				execution.display()
			))
		);
		assert_eq!(
			local(RunState::Failed, None),
			Some(format!("run failed: no reason recorded (details in {})", execution.display()))
		);
		assert_eq!(local(RunState::Completed, None), None);
		assert_eq!(local(RunState::CompletedWithFailures, None), None);
		assert_eq!(local(RunState::Stopped, Some("run cancelled")), None);
	}

	#[test]
	fn failure_reasons_fit_on_one_safe_line() {
		assert_eq!(
			one_line("plugin failed:\nfirst line\r\nsecond line\n"),
			"plugin failed: ⏎ first line ⏎ second line"
		);
		assert_eq!(
			one_line("\u{1b}[31mred\u{1b}[0m \u{1b}]0;title\u{7} tab\there \u{202e}txt"),
			"red \\u{1b}]0;title\\u{7} tab\\u{9}here \\u{202e}txt"
		);
		let long = "x".repeat(100_000);
		let line = one_line(&long);
		assert_eq!(line, format!("{}…", "x".repeat(MAX_REASON_CHARS)));
		assert_eq!(one_line(&"x".repeat(MAX_REASON_CHARS)), "x".repeat(MAX_REASON_CHARS));
		assert_eq!(one_line("été 🚀"), "été 🚀");
		// The cap applies to the rendered line and never cuts an escape in half.
		let escaped = one_line(&"\u{202e}".repeat(1_000));
		assert_eq!(escaped, format!("{}…", "\\u{202e}".repeat(MAX_REASON_CHARS / 8)));
		assert!(escaped.chars().count() <= MAX_REASON_CHARS + 1);
		let mixed = one_line(&"a\u{1}\n".repeat(1_000));
		// Each repeat renders as 9 characters; after 33 of them and an `a` (298), `\u{1}` no
		// longer fits whole.
		assert_eq!(mixed, format!("{}a…", "a\\u{1} ⏎ ".repeat(33)));
		assert!(mixed.chars().count() <= MAX_REASON_CHARS + 1);
		let reason = failure_reason(
			RunLocation::Local,
			&Outcome {
				artifact_dir: PathBuf::from("runs/run-1"),
				exit_code: 1,
				state: RunState::Failed,
				error: Some(format!("a\nb\u{1b}[1m{long}")),
				teardown: None,
			},
		)
		.unwrap();
		assert!(!reason.contains('\n') && !reason.contains('\u{1b}'));
		assert!(reason.starts_with("run failed: a ⏎ bxxx"));
		assert!(reason.len() < MAX_REASON_CHARS + 100);
	}

	#[test]
	fn command_line_enforces_run_argument_constraints() {
		let parses = |args: &[&str]| Cli::try_parse_from([&["polkameter"], args].concat()).is_ok();
		assert!(parses(&["validate", "plan.xml"]));
		assert!(parses(&["run", "plan.xml", "--output", "runs"]));
		let cli = Cli::try_parse_from([
			"polkameter",
			"run",
			"plan.xml",
			"--output",
			"runs",
			"--fail-on-verdict",
		])
		.unwrap();
		assert!(matches!(cli.command, Command::Run { fail_on_verdict: true, .. }));
		let cli =
			Cli::try_parse_from(["polkameter", "run", "plan.xml", "--output", "runs"]).unwrap();
		assert!(matches!(cli.command, Command::Run { fail_on_verdict: false, .. }));
		assert!(parses(&["agent", "serve"]));
		assert!(parses(&["agent", "serve", "--keep-runs", "5", "--shutdown-grace", "0"]));
		assert!(!parses(&["agent", "serve", "--keep-runs", "0"]));
		assert!(parses(&["run", "plan.xml", "--output", "runs", "--shutdown-grace", "5"]));
		assert!(!parses(&["run", "plan.xml", "--output", "runs", "--shutdown-grace", "-1"]));
		assert!(!parses(&["run", "plan.xml"]));
		assert!(!parses(&["run", "plan.xml", "--output", "runs", "--remote-token-env", "T"]));
		assert!(parses(&[
			"run",
			"plan.xml",
			"--remote",
			"http://127.0.0.1:9901",
			"--remote-token-env",
			"T"
		]));
		assert!(!parses(&[
			"run",
			"plan.xml",
			"--remote",
			"http://127.0.0.1:9901",
			"--credential-env",
			"S"
		]));
	}
}
