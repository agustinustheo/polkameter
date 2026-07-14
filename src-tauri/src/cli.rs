use std::{io::Write, path::PathBuf, sync::Arc, time::Duration};

use clap::{Args, Parser, Subcommand, ValueEnum};
use serde_json::{json, Value};

use crate::{
	application::{self, RemoteRunnerTarget, RunEvent, RunEventSink, RunStatus, RunnerState},
	artifacts,
};

const EVENT_VERSION: u32 = 1;

#[derive(Debug, Parser)]
#[command(name = "polkameter", about = "Headless Polkadot SDK stress testing")]
struct Cli {
	#[command(subcommand)]
	command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
	/// Parse and structurally validate a scenario without connecting to a chain.
	Validate {
		scenario: PathBuf,
		#[arg(long, value_enum, default_value_t = OutputFormat::Human)]
		format: OutputFormat,
	},
	/// Validate live metadata, SCALE encoding, and signer readiness without submitting
	/// transactions.
	Preflight {
		scenario: PathBuf,
		#[command(flatten)]
		signer: SignerArgs,
		#[arg(long, value_enum, default_value_t = OutputFormat::Human)]
		format: OutputFormat,
	},
	/// Preflight and execute a scenario locally or through an authenticated remote agent.
	Run {
		scenario: PathBuf,
		/// Local artifact root. Required unless --remote is used because remote agents own their
		/// artifact root.
		#[arg(long, required_unless_present = "remote")]
		output: Option<PathBuf>,
		#[command(flatten)]
		signer: SignerArgs,
		/// Remote runner endpoint. It must use HTTPS or be a loopback HTTP tunnel.
		#[arg(long)]
		remote: Option<String>,
		/// Environment variable holding the remote-agent bearer token.
		#[arg(long, requires = "remote")]
		remote_token_env: Option<String>,
		#[arg(long, value_enum, default_value_t = OutputFormat::Human)]
		format: OutputFormat,
	},
	/// Read and validate a portable Polkameter artifact directory.
	Report {
		artifact_directory: PathBuf,
		#[arg(long, value_enum, default_value_t = OutputFormat::Human)]
		format: OutputFormat,
	},
	/// Start an authenticated, loopback-only remote runner agent.
	Agent {
		#[command(subcommand)]
		command: AgentCommand,
	},
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

#[derive(Debug, Args)]
struct SignerArgs {
	/// Named signer profile stored in the operating-system credential vault.
	#[arg(long, conflicts_with = "signer_env")]
	signer_profile: Option<String>,
	/// Name of an environment variable containing the signer SURI.
	#[arg(long, conflicts_with = "signer_profile")]
	signer_env: Option<String>,
}

#[derive(Debug)]
enum CliError {
	Invalid(String),
	Preflight(String),
	Runtime(String),
	RunFailed(Box<RunStatus>),
	Interrupted,
}

impl CliError {
	fn exit_code(&self) -> i32 {
		match self {
			Self::Invalid(_) => 2,
			Self::Preflight(_) => 3,
			Self::Runtime(_) => 1,
			Self::RunFailed(_) => 4,
			Self::Interrupted => 130,
		}
	}
}

impl std::fmt::Display for CliError {
	fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self {
			Self::Invalid(message) | Self::Preflight(message) | Self::Runtime(message) => {
				formatter.write_str(message)
			},
			Self::RunFailed(status) => formatter.write_str(
				status
					.message
					.as_deref()
					.unwrap_or("run completed with failed or timed-out samples"),
			),
			Self::Interrupted => formatter.write_str("run interrupted"),
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
		Command::Validate { scenario, format } => validate(scenario, format),
		Command::Preflight { scenario, signer, format } => {
			preflight(scenario, signer, format).await
		},
		Command::Run { scenario, output, signer, remote, remote_token_env, format } => {
			run(scenario, output, signer, remote, remote_token_env, format).await
		},
		Command::Report { artifact_directory, format } => report(artifact_directory, format),
		Command::Agent { command } => agent(command).await,
	}
}

fn validate(path: PathBuf, format: OutputFormat) -> Result<i32, CliError> {
	let document = application::load_scenario_document(path).map_err(CliError::Invalid)?;
	let issues = document.validate();
	let samples = estimated_samples(&document);
	let payload = json!({
		"version": EVENT_VERSION,
		"event": "validation",
		"valid": issues.is_empty(),
		"issues": issues,
		"estimatedSamples": samples,
	});
	write_output(
		format,
		&payload,
		if issues.is_empty() {
			format!("Scenario is valid ({samples} scheduled samples).")
		} else {
			format!("Scenario is invalid:\n{}", issue_lines(&document.validate()))
		},
	);
	if document.validate().is_empty() {
		Ok(0)
	} else {
		Err(CliError::Invalid("scenario validation failed".into()))
	}
}

async fn preflight(
	path: PathBuf,
	signer: SignerArgs,
	format: OutputFormat,
) -> Result<i32, CliError> {
	let mut document = application::load_scenario_document(path).map_err(CliError::Invalid)?;
	resolve_signer(&mut document, &signer)?;
	let run_id = artifacts::new_run_id();
	let report = application::preflight_scenario(&document, &run_id)
		.await
		.map_err(CliError::Preflight)?;
	write_output(
		format,
		&json!({ "version": EVENT_VERSION, "event": "preflight", "report": report }),
		format!(
			"Preflight passed for {} (spec {}, {} resolved samples).",
			document.chain.endpoint, report.spec_version, report.resolved_sample_count
		),
	);
	Ok(0)
}

async fn run(
	path: PathBuf,
	output: Option<PathBuf>,
	signer: SignerArgs,
	remote: Option<String>,
	remote_token_env: Option<String>,
	format: OutputFormat,
) -> Result<i32, CliError> {
	let mut document = application::load_scenario_document(path).map_err(CliError::Invalid)?;
	if let Some(endpoint) = remote {
		if signer.signer_env.is_some() {
			return Err(CliError::Invalid(
				"--signer-env cannot be used with --remote; configure the signer on the agent"
					.into(),
			));
		}
		if let Some(profile) = signer.signer_profile.as_deref() {
			crate::validate_signer_profile(profile).map_err(CliError::Invalid)?;
			document.signer_source.profile = profile.into();
		}
		let target = remote_target(endpoint, remote_token_env)?;
		return run_remote(target, document, format).await;
	}
	let output = output.expect("clap requires --output when --remote is absent");
	resolve_signer(&mut document, &signer)?;
	let run_id = artifacts::new_run_id();
	let preflight = application::preflight_scenario(&document, &run_id)
		.await
		.map_err(CliError::Preflight)?;
	write_output(
		format,
		&json!({ "version": EVENT_VERSION, "event": "preflight", "report": preflight }),
		"Preflight passed; arming local run.".into(),
	);

	let state = Arc::new(RunnerState::default());
	let sink = Arc::new(ConsoleEventSink { format, run_id: run_id.clone() });
	application::start_local_run(
		document,
		output.display().to_string(),
		run_id,
		sink,
		state.clone(),
		"Polkameter run started through the command-line interface\n",
	)
	.await
	.map_err(CliError::Runtime)?;
	finish_local_run(state, format).await
}

async fn run_remote(
	target: RemoteRunnerTarget,
	document: crate::scenario::ScenarioDocument,
	format: OutputFormat,
) -> Result<i32, CliError> {
	let run_id = artifacts::new_run_id();
	let preflight = application::preflight_remote_run(&target, document.clone(), run_id.clone())
		.await
		.map_err(CliError::Preflight)?;
	write_output(
		format,
		&json!({ "version": EVENT_VERSION, "event": "preflight", "report": preflight }),
		"Remote preflight passed; arming remote run.".into(),
	);
	application::start_remote_run(&target, document, run_id.clone())
		.await
		.map_err(CliError::Runtime)?;
	let mut interrupt = Box::pin(tokio::signal::ctrl_c());
	let mut stopping = false;
	loop {
		let status = application::remote_run_status(&target, &run_id)
			.await
			.map_err(CliError::Runtime)?;
		write_status(format, &status);
		if is_finished(&status) {
			let report = application::read_remote_run_report(&target, &run_id)
				.await
				.map_err(CliError::Runtime)?;
			write_output(
				format,
				&json!({ "version": EVENT_VERSION, "event": "artifact-written", "runId": run_id, "artifactDir": status.artifact_dir, "summary": report.summary }),
				"Remote artifacts validated by the agent.".into(),
			);
			return exit_for_status(status, stopping);
		}
		tokio::select! {
			result = &mut interrupt, if !stopping => {
				result.map_err(|error| CliError::Runtime(format!("could not receive Ctrl-C: {error}")))?;
				application::stop_remote_run(&target, &run_id).await.map_err(CliError::Runtime)?;
				stopping = true;
			}
			_ = tokio::time::sleep(Duration::from_millis(250)) => {}
		}
	}
}

async fn finish_local_run(state: Arc<RunnerState>, format: OutputFormat) -> Result<i32, CliError> {
	let mut interrupt = Box::pin(tokio::signal::ctrl_c());
	let mut stopping = false;
	loop {
		let status = application::run_status(state.clone()).await;
		if is_finished(&status) {
			if let Some(artifact_dir) = &status.artifact_dir {
				let report =
					application::read_run_report(artifact_dir).map_err(CliError::Runtime)?;
				write_output(
					format,
					&json!({ "version": EVENT_VERSION, "event": "artifact-written", "runId": status.run_id, "artifactDir": artifact_dir, "summary": report.summary }),
					format!("Artifacts validated: {artifact_dir}"),
				);
			}
			return exit_for_status(status, stopping);
		}
		tokio::select! {
			result = &mut interrupt, if !stopping => {
				result.map_err(|error| CliError::Runtime(format!("could not receive Ctrl-C: {error}")))?;
				application::stop_local_run(state.clone()).await.map_err(CliError::Runtime)?;
				stopping = true;
			}
			_ = tokio::time::sleep(Duration::from_millis(250)) => {}
		}
	}
}

fn report(path: PathBuf, format: OutputFormat) -> Result<i32, CliError> {
	let report = application::read_run_report(&path).map_err(CliError::Runtime)?;
	write_output(
		format,
		&json!({ "version": EVENT_VERSION, "event": "report", "artifactDir": path, "summary": report.summary, "plots": report.plots.iter().map(|plot| &plot.name).collect::<Vec<_>>() }),
		report.summary,
	);
	Ok(0)
}

async fn agent(command: AgentCommand) -> Result<i32, CliError> {
	match command {
		AgentCommand::Serve { bind, token_env, output_root } => {
			application::validate_environment_variable_name(&token_env)
				.map_err(CliError::Invalid)?;
			let token = std::env::var(&token_env).map_err(|_| {
				CliError::Invalid(format!(
					"agent token environment variable {token_env} is not set"
				))
			})?;
			application::serve_agent(&bind, token, output_root)
				.await
				.map_err(CliError::Runtime)?;
			Ok(0)
		},
	}
}

fn resolve_signer(
	document: &mut crate::scenario::ScenarioDocument,
	signer: &SignerArgs,
) -> Result<(), CliError> {
	application::resolve_signer_source(
		document,
		signer.signer_profile.as_deref(),
		signer.signer_env.as_deref(),
	)
	.map_err(CliError::Preflight)
}

fn remote_target(
	endpoint: String,
	token_env: Option<String>,
) -> Result<RemoteRunnerTarget, CliError> {
	let token_env = token_env
		.ok_or_else(|| CliError::Invalid("--remote-token-env is required with --remote".into()))?;
	application::validate_environment_variable_name(&token_env).map_err(CliError::Invalid)?;
	let bearer_token = std::env::var(&token_env).map_err(|_| {
		CliError::Invalid(format!("remote token environment variable {token_env} is not set"))
	})?;
	let target = RemoteRunnerTarget { endpoint, bearer_token };
	target.validate().map_err(CliError::Invalid)?;
	Ok(target)
}

fn estimated_samples(document: &crate::scenario::ScenarioDocument) -> u64 {
	document
		.thread_groups
		.iter()
		.map(|group| u64::from(group.users) * group.samplers.len() as u64)
		.sum()
}

fn issue_lines(issues: &[crate::scenario::ValidationIssue]) -> String {
	issues
		.iter()
		.map(|issue| format!("- {}: {}", issue.field, issue.message))
		.collect::<Vec<_>>()
		.join("\n")
}

fn is_finished(status: &RunStatus) -> bool {
	matches!(status.state.as_str(), "completed" | "completed_with_failures" | "failed" | "stopped")
}

fn exit_for_status(status: RunStatus, interrupted: bool) -> Result<i32, CliError> {
	if interrupted || status.state == "stopped" {
		return Err(CliError::Interrupted);
	}
	match status.state.as_str() {
		"completed" => Ok(0),
		"completed_with_failures" => Err(CliError::RunFailed(Box::new(status))),
		"failed" => Err(CliError::Runtime(status.message.unwrap_or_else(|| "run failed".into()))),
		_ => Err(CliError::Runtime(format!("unexpected terminal run state {}", status.state))),
	}
}

fn write_output(format: OutputFormat, json_value: &Value, human: String) {
	match format {
		OutputFormat::Human => eprintln!("{human}"),
		OutputFormat::Json => write_json_line(json_value),
	}
}

fn write_status(format: OutputFormat, status: &RunStatus) {
	write_output(
		format,
		&json!({ "version": EVENT_VERSION, "event": "run-status", "runId": status.run_id, "status": status }),
		format!(
			"Run {}: {} ({}/{} successful)",
			status.run_id.as_deref().unwrap_or("unknown"),
			status.state,
			status.successful_samples,
			status.completed_samples
		),
	);
}

fn write_json_line(value: &Value) {
	let mut stdout = std::io::stdout().lock();
	let _ = serde_json::to_writer(&mut stdout, value);
	let _ = stdout.write_all(b"\n");
	let _ = stdout.flush();
}

struct ConsoleEventSink {
	format: OutputFormat,
	run_id: String,
}

impl RunEventSink for ConsoleEventSink {
	fn emit(&self, event: RunEvent) {
		let value = serde_json::to_value(event).unwrap_or_else(
			|error| json!({ "kind": "serialization_error", "message": error.to_string() }),
		);
		let event_name = if value.get("kind") == Some(&Value::String("status".into())) {
			"run-status"
		} else if value.get("success") == Some(&Value::Bool(false)) {
			"sample-failure"
		} else {
			"sample"
		};
		write_output(
			self.format,
			&json!({ "version": EVENT_VERSION, "event": event_name, "runId": self.run_id, "data": value }),
			format!("Run {}: {event_name}", self.run_id),
		);
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn command_line_parses_the_supported_workflows() {
		assert!(Cli::try_parse_from(["polkameter", "validate", "scenario.json"]).is_ok());
		assert!(
			Cli::try_parse_from(["polkameter", "run", "scenario.json", "--output", "runs"]).is_ok()
		);
		assert!(Cli::try_parse_from(["polkameter", "agent", "serve"]).is_ok());
	}

	#[test]
	fn terminal_states_have_stable_exit_codes() {
		assert!(matches!(
			exit_for_status(RunStatus { state: "completed".into(), ..RunStatus::default() }, false),
			Ok(0)
		));
		assert_eq!(
			exit_for_status(
				RunStatus { state: "completed_with_failures".into(), ..RunStatus::default() },
				false
			)
			.expect_err("failure state")
			.exit_code(),
			4
		);
		assert_eq!(
			exit_for_status(RunStatus { state: "stopped".into(), ..RunStatus::default() }, true)
				.expect_err("stopped state")
				.exit_code(),
			130
		);
	}

	#[test]
	fn json_events_do_not_include_signer_material() {
		let payload = json!({ "version": EVENT_VERSION, "event": "run-status", "runId": "run-1" });
		assert!(!payload.to_string().contains("//Alice"));
	}
}
