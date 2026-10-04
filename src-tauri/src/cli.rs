//! The headless CLI: validate, preflight, run and report XML plans, manage installed plugins,
//! and serve or use a remote agent.

use std::{io::Write, path::PathBuf, sync::Arc, time::Duration};

use clap::{Parser, Subcommand, ValueEnum};
use polkameter_engine::{plan::Plan, plugins::Registry};
use serde_json::{json, Value};

use crate::remote::{self, RemoteRunnerTarget};

#[derive(Debug, Parser)]
#[command(name = "polkameter", about = "Headless Polkadot SDK load testing")]
struct Cli {
	#[command(subcommand)]
	command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
	/// Install plugins and configure this host's credentials, topologies and capabilities.
	Plugin {
		#[command(subcommand)]
		command: PluginCommand,
	},
	/// Parse and structurally validate a plan without connecting to a chain.
	Validate {
		plan: PathBuf,
		#[arg(long, value_enum, default_value_t = OutputFormat::Human)]
		format: OutputFormat,
	},
	/// Check installed plugins, required evidence and read-only steps without submitting.
	Preflight {
		plan: PathBuf,
		/// Environment variable for the plan's only credential, instead of its configured profile.
		#[arg(long)]
		credential_env: Option<String>,
		#[arg(long, value_enum, default_value_t = OutputFormat::Human)]
		format: OutputFormat,
	},
	/// Run a plan locally or through an authenticated remote agent.
	Run {
		plan: PathBuf,
		/// Local artifact root. Required unless --remote is used because remote agents own their
		/// artifact root.
		#[arg(long, required_unless_present = "remote")]
		output: Option<PathBuf>,
		/// Environment variable for the plan's only credential, instead of its configured profile.
		#[arg(long, conflicts_with = "remote")]
		credential_env: Option<String>,
		/// Remote runner endpoint. It must use HTTPS or be a loopback HTTP tunnel.
		#[arg(long)]
		remote: Option<String>,
		/// Environment variable holding the remote-agent bearer token.
		#[arg(long, requires = "remote")]
		remote_token_env: Option<String>,
		#[arg(long, value_enum, default_value_t = OutputFormat::Human)]
		format: OutputFormat,
	},
	/// Regenerate the checks, summary and plots of a run directory from its files.
	Report {
		artifact_directory: PathBuf,
		#[arg(long, value_enum, default_value_t = OutputFormat::Human)]
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
	Capability { name: String },
	/// Resolve manifests and export requirements without connecting or submitting.
	Inspect { scenario: PathBuf },
	/// Register an executable after reading its manifest and pinning its checksum.
	Install { executable: PathBuf },
	/// List installed plugin packages.
	List,
	/// Map a credential profile to an environment variable on this host.
	Credential { profile: String, env: String },
	/// Register a local Zombienet topology under a portable alias.
	Topology { name: String, path: PathBuf },
}

#[derive(Debug, Subcommand)]
enum AgentCommand {
	/// Serve remote run requests through the versioned Polkameter agent protocol.
	Serve {
		#[arg(long, default_value = "127.0.0.1:9901")]
		bind: String,
		#[arg(long, default_value = "POLKAMETER_AGENT_TOKEN")]
		token_env: String,
		#[arg(long, default_value = "target/polkameter-agent-runs")]
		output_root: String,
	},
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
enum OutputFormat {
	#[default]
	Human,
	Json,
}

#[derive(Debug)]
enum CliError {
	Invalid(String),
	Preflight(String),
	Runtime(String),
}

impl CliError {
	fn exit_code(&self) -> i32 {
		match self {
			Self::Invalid(_) => 2,
			Self::Preflight(_) => 3,
			Self::Runtime(_) => 1,
		}
	}
}

impl std::fmt::Display for CliError {
	fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			Self::Invalid(message) | Self::Preflight(message) | Self::Runtime(message) => {
				formatter.write_str(message)
			},
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
	let runtime = match tokio::runtime::Runtime::new() {
		Ok(runtime) => runtime,
		Err(error) => {
			eprintln!("Polkameter could not start its async runtime: {error}");
			return 1;
		},
	};
	match runtime.block_on(execute(cli)) {
		Ok(code) => code,
		Err(error) => {
			eprintln!("Polkameter: {error}");
			error.exit_code()
		},
	}
}

async fn execute(cli: Cli) -> Result<i32, CliError> {
	match cli.command {
		Command::Plugin { command } => plugin_command(command).await,
		Command::Validate { plan, format } => {
			let plan = load(&plan)?;
			write_result(
				format,
				&json!({"event":"validation","valid":true,"plan":plan.name}),
				"Plan is structurally valid. Preflight checks the installed plugins.".into(),
			);
			Ok(0)
		},
		Command::Preflight { plan, credential_env, format } => {
			let plan = load(&plan)?;
			let registry = registry(&plan, credential_env)?;
			let result = polkameter_engine::execute::preflight(&plan, &registry)
				.await
				.map_err(|e| CliError::Preflight(e.to_string()))?;
			write_result(format, &result, "Preflight passed.".into());
			Ok(0)
		},
		Command::Run { plan: path, output, credential_env, remote, remote_token_env, format } => {
			let plan = load(&path)?;
			match remote {
				Some(endpoint) => {
					let xml = std::fs::read_to_string(path)
						.map_err(|e| CliError::Invalid(e.to_string()))?;
					run_plugin_remote(remote_target(endpoint, remote_token_env)?, xml, format).await
				},
				None => {
					let output = output.expect("clap requires --output when --remote is absent");
					let registry = registry(&plan, credential_env)?;
					run_local(plan, registry, output, format).await
				},
			}
		},
		Command::Report { artifact_directory, format } => report(artifact_directory, format),
		Command::Agent { command } => agent(command).await,
	}
}

fn load(path: &std::path::Path) -> Result<Plan, CliError> {
	let xml = std::fs::read_to_string(path).map_err(|e| CliError::Invalid(e.to_string()))?;
	Plan::parse(&xml).map_err(|e| CliError::Invalid(e.to_string()))
}

/// This host's registry; `credential_env` replaces the profile of the plan's only credential.
fn registry(plan: &Plan, credential_env: Option<String>) -> Result<Registry, CliError> {
	let mut registry = Registry::load().map_err(|e| CliError::Invalid(e.to_string()))?;
	if let Some(variable) = credential_env {
		let [credential] = plan.credentials.entries.as_slice() else {
			return Err(CliError::Invalid(
				"--credential-env requires exactly one plan credential".into(),
			));
		};
		registry.credentials.insert(credential.profile.clone(), variable);
	}
	Ok(registry)
}

async fn run_local(
	plan: Plan,
	registry: Registry,
	output: PathBuf,
	format: OutputFormat,
) -> Result<i32, CliError> {
	let cancel = tokio_util::sync::CancellationToken::new();
	let signal = cancel.clone();
	let interrupt = tokio::spawn(async move {
		if tokio::signal::ctrl_c().await.is_ok() {
			signal.cancel();
		}
	});
	let sink = Arc::new(move |event: Value| {
		if format == OutputFormat::Json {
			write_json_line(&event);
		} else if event["event"] == "phase" {
			eprintln!("Phase: {}", event["phase"].as_str().unwrap_or("unknown"));
		}
	});
	let result = polkameter_engine::execute::run(plan, registry, &output, cancel, sink).await;
	interrupt.abort();
	let outcome = result
		.and_then(crate::plugin_application::finalize_report)
		.map_err(|e| CliError::Runtime(e.to_string()))?;
	write_result(
		format,
		&json!({"event":"artifact-written","outcome":outcome}),
		format!("Run {}. Artifacts: {}", outcome.state, outcome.artifact_dir.display()),
	);
	Ok(outcome.exit_code)
}

fn report(path: PathBuf, format: OutputFormat) -> Result<i32, CliError> {
	let runtime = |e: anyhow::Error| CliError::Runtime(e.to_string());
	polkameter_engine::artifacts::write_samples(&path).map_err(runtime)?;
	crate::report::write(&path).map_err(CliError::Runtime)?;
	if path.join("summary.json").is_file() {
		polkameter_engine::measurement::check(&path).map_err(runtime)?;
	}
	let outcome: polkameter_engine::execute::Outcome = serde_json::from_slice(
		&std::fs::read(path.join("execution.json"))
			.map_err(|e| CliError::Runtime(e.to_string()))?,
	)
	.map_err(|e| CliError::Runtime(e.to_string()))?;
	let summary = std::fs::read_to_string(path.join("summary.md"))
		.map_err(|e| CliError::Runtime(e.to_string()))?;
	write_result(format, &json!({"event":"report","outcome":outcome,"summary":summary}), summary);
	Ok(0)
}

async fn agent(command: AgentCommand) -> Result<i32, CliError> {
	match command {
		AgentCommand::Serve { bind, token_env, output_root } => {
			let token = std::env::var(&token_env).map_err(|_| {
				CliError::Invalid(format!(
					"agent token environment variable {token_env} is not set"
				))
			})?;
			remote::serve(&bind, token, output_root).await.map_err(CliError::Runtime)?;
			Ok(0)
		},
	}
}

fn remote_target(
	endpoint: String,
	token_env: Option<String>,
) -> Result<RemoteRunnerTarget, CliError> {
	let token_env = token_env
		.ok_or_else(|| CliError::Invalid("--remote-token-env is required with --remote".into()))?;
	let bearer_token = std::env::var(&token_env).map_err(|_| {
		CliError::Invalid(format!("remote token environment variable {token_env} is not set"))
	})?;
	let target = RemoteRunnerTarget { endpoint, bearer_token };
	target.validate().map_err(CliError::Invalid)?;
	Ok(target)
}

/// A JSON line in JSON mode, else the human text on stdout.
fn write_result(format: OutputFormat, json_value: &Value, human: String) {
	if format == OutputFormat::Json {
		write_json_line(json_value);
	} else {
		println!("{human}");
	}
}

fn write_json_line(value: &Value) {
	let mut stdout = std::io::stdout().lock();
	let _ = serde_json::to_writer(&mut stdout, value);
	let _ = stdout.write_all(b"\n");
	let _ = stdout.flush();
}

async fn plugin_command(command: PluginCommand) -> Result<i32, CliError> {
	use polkameter_engine::plugins::Registry;
	let result = async {
		let path = Registry::path()?;
		let mut registry = Registry::read(&path)?;
		let changed = !matches!(&command, PluginCommand::List | PluginCommand::Inspect { .. });
		let result = match command {
			PluginCommand::Install { executable } => {
				serde_json::to_value(registry.install(&executable).await?)?
			},
			PluginCommand::List => serde_json::to_value(&registry.plugins)?,
			PluginCommand::Capability { name } => {
				if !registry.capabilities.contains(&name) {
					registry.capabilities.push(name);
				}
				json!({"configured":true})
			},
			PluginCommand::Inspect { scenario } => {
				let xml = std::fs::read_to_string(scenario)?;
				polkameter_engine::execute::inspect(
					&polkameter_engine::plan::Plan::parse(&xml)?,
					&registry,
				)
				.await?
			},
			PluginCommand::Credential { profile, env } => {
				registry.credentials.insert(profile, env);
				json!({"configured":true})
			},
			PluginCommand::Topology { name, path } => {
				registry.topologies.insert(name, path.canonicalize()?);
				json!({"configured":true})
			},
		};
		if changed {
			registry.write(&path)?;
		}
		Ok::<_, anyhow::Error>(result)
	}
	.await
	.map_err(|e| CliError::Invalid(e.to_string()))?;
	println!(
		"{}",
		serde_json::to_string_pretty(&result).map_err(|e| CliError::Runtime(e.to_string()))?
	);
	Ok(0)
}
async fn run_plugin_remote(
	target: RemoteRunnerTarget,
	xml: String,
	format: OutputFormat,
) -> Result<i32, CliError> {
	let started = remote::plugin_start(&target, xml).await.map_err(CliError::Runtime)?;
	let mut interrupt = Box::pin(tokio::signal::ctrl_c());
	let mut stopped = false;
	loop {
		let status =
			remote::plugin_status(&started.id, &target).await.map_err(CliError::Runtime)?;
		if status.state != "running" {
			if let Some(outcome) = status.outcome {
				write_result(
					format,
					&json!({"event":"artifact-written","outcome":outcome}),
					format!(
						"Remote run {}. Artifacts: {}",
						outcome.state,
						outcome.artifact_dir.display()
					),
				);
				return Ok(outcome.exit_code);
			}
			return Err(CliError::Runtime(
				status.error.unwrap_or_else(|| "remote run produced no outcome".into()),
			));
		}
		tokio::select! {result=&mut interrupt,if !stopped=>{result.map_err(|e|CliError::Runtime(e.to_string()))?;remote::plugin_stop(&started.id,&target).await.map_err(CliError::Runtime)?;stopped=true;},_=tokio::time::sleep(Duration::from_millis(300))=>{}}
	}
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
		assert_eq!(report(directory.clone(), OutputFormat::Json).unwrap(), 0);
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
		assert!(directory.join("summary.md").is_file());
		assert_eq!(replayed["loss"], recorded["loss"]);
		std::fs::remove_dir_all(directory).unwrap();
	}

	#[test]
	fn command_line_enforces_run_argument_constraints() {
		let parses = |args: &[&str]| Cli::try_parse_from([&["polkameter"], args].concat()).is_ok();
		assert!(parses(&["validate", "plan.xml"]));
		assert!(parses(&["run", "plan.xml", "--output", "runs"]));
		assert!(parses(&["agent", "serve"]));
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
