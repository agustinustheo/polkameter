use std::{
	io::Write,
	path::PathBuf,
	sync::Arc,
	time::{Duration, Instant},
};

use clap::{Args, Parser, Subcommand, ValueEnum};
use serde_json::{json, Value};

use crate::{
	application,
	artifacts::{self, RunOrigin},
	preflight,
	remote::{self, RemoteRunnerTarget},
	report,
	runner::{self, RunEvent, RunEventSink, RunStatus, RunnerState},
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
	let payload = validation_payload(&issues, samples);
	write_result(
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
	let report = preflight::preflight(&document, &run_id).await.map_err(CliError::Preflight)?;
	write_result(
		format,
		&preflight_payload(&report),
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
	let preflight = preflight::preflight(&document, &run_id).await.map_err(CliError::Preflight)?;
	write_progress(
		format,
		&preflight_payload(&preflight),
		"Preflight passed; arming local run.".into(),
	);

	let state = Arc::new(RunnerState::default());
	let sink = Arc::new(ConsoleEventSink { format, run_id: run_id.clone() });
	runner::start_with_command(
		document,
		output.display().to_string(),
		run_id,
		sink,
		state.clone(),
		RunOrigin::Cli,
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
	let preflight = remote::preflight(&target, document.clone(), run_id.clone())
		.await
		.map_err(CliError::Preflight)?;
	write_progress(
		format,
		&preflight_payload(&preflight),
		"Remote preflight passed; arming remote run.".into(),
	);
	remote::start(&target, document, run_id.clone())
		.await
		.map_err(CliError::Runtime)?;
	let mut interrupt = Box::pin(tokio::signal::ctrl_c());
	let mut progress = HumanProgressReporter::default();
	let mut stopping = false;
	loop {
		let status = remote::status(&target, &run_id).await.map_err(CliError::Runtime)?;
		write_status(format, &status, &mut progress);
		if is_finished(&status) {
			let report =
				remote::read_remote_report(&target, &run_id).await.map_err(CliError::Runtime)?;
			write_result(
				format,
				&json!({ "version": EVENT_VERSION, "event": "artifact-written", "runId": run_id, "artifactDir": status.artifact_dir, "summary": report.summary }),
				final_artifact_output(status.artifact_dir.as_deref(), &report.summary, true),
			);
			return exit_for_status(status, stopping);
		}
		tokio::select! {
			result = &mut interrupt, if !stopping => {
				result.map_err(|error| CliError::Runtime(format!("could not receive Ctrl-C: {error}")))?;
				remote::stop(&target, &run_id).await.map_err(CliError::Runtime)?;
				stopping = true;
			}
			_ = tokio::time::sleep(Duration::from_millis(250)) => {}
		}
	}
}

async fn finish_local_run(state: Arc<RunnerState>, format: OutputFormat) -> Result<i32, CliError> {
	let mut interrupt = Box::pin(tokio::signal::ctrl_c());
	let mut stopping = false;
	let mut progress = HumanProgressReporter::default();
	loop {
		let status = runner::status(state.clone()).await;
		if format == OutputFormat::Human {
			write_human_status(&status, &mut progress);
		}
		if is_finished(&status) {
			if let Some(artifact_dir) = &status.artifact_dir {
				let report = report::read_dashboard(std::path::Path::new(artifact_dir))
					.map_err(CliError::Runtime)?;
				write_result(
					format,
					&json!({ "version": EVENT_VERSION, "event": "artifact-written", "runId": status.run_id, "artifactDir": artifact_dir, "summary": report.summary }),
					final_artifact_output(Some(artifact_dir), &report.summary, false),
				);
			}
			return exit_for_status(status, stopping);
		}
		tokio::select! {
			result = &mut interrupt, if !stopping => {
				result.map_err(|error| CliError::Runtime(format!("could not receive Ctrl-C: {error}")))?;
				runner::stop(state.clone()).await.map_err(CliError::Runtime)?;
				stopping = true;
			}
			_ = tokio::time::sleep(Duration::from_millis(250)) => {}
		}
	}
}

fn report(path: PathBuf, format: OutputFormat) -> Result<i32, CliError> {
	let report = report::read_dashboard(&path).map_err(CliError::Runtime)?;
	write_result(
		format,
		&json!({ "version": EVENT_VERSION, "event": "report", "artifactDir": path, "summary": report.summary, "plots": report.plots.iter().map(|plot| &plot.name).collect::<Vec<_>>() }),
		report.summary,
	);
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

fn validation_payload(issues: &[crate::scenario::ValidationIssue], samples: u64) -> Value {
	json!({
		"version": EVENT_VERSION,
		"event": "validation",
		"valid": issues.is_empty(),
		"issues": issues,
		"estimatedSamples": samples,
	})
}

fn preflight_payload(report: &crate::preflight::PreflightReport) -> Value {
	json!({ "version": EVENT_VERSION, "event": "preflight", "report": report })
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OutputKind {
	Result,
	Progress,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum OutputDestination {
	Stdout,
	Stderr,
}

fn output_destination(format: OutputFormat, kind: OutputKind) -> OutputDestination {
	match (format, kind) {
		(OutputFormat::Json, _) | (OutputFormat::Human, OutputKind::Result) => {
			OutputDestination::Stdout
		},
		(OutputFormat::Human, OutputKind::Progress) => OutputDestination::Stderr,
	}
}

fn write_result(format: OutputFormat, json_value: &Value, human: String) {
	write_output(format, OutputKind::Result, json_value, human);
}

fn write_progress(format: OutputFormat, json_value: &Value, human: String) {
	write_output(format, OutputKind::Progress, json_value, human);
}

fn write_output(format: OutputFormat, kind: OutputKind, json_value: &Value, human: String) {
	if format == OutputFormat::Json {
		write_json_line(json_value);
		return;
	}
	match output_destination(format, kind) {
		OutputDestination::Stdout => println!("{human}"),
		OutputDestination::Stderr => eprintln!("{human}"),
	}
}

fn write_status(format: OutputFormat, status: &RunStatus, progress: &mut HumanProgressReporter) {
	match format {
		OutputFormat::Json => write_json_line(
			&json!({ "version": EVENT_VERSION, "event": "run-status", "runId": status.run_id, "status": status }),
		),
		OutputFormat::Human => write_human_status(status, progress),
	}
}

fn write_human_status(status: &RunStatus, progress: &mut HumanProgressReporter) {
	if progress.should_emit(status) {
		eprintln!("{}", human_status_line(status));
	}
}

fn human_status_line(status: &RunStatus) -> String {
	let mut details = format!(
		"{} successful / {} completed",
		status.successful_samples, status.completed_samples
	);
	if status.failed_samples > 0 {
		details.push_str(&format!(", {} failed", status.failed_samples));
	}
	if status.timed_out_samples > 0 {
		details.push_str(&format!(", {} timed out", status.timed_out_samples));
	}
	format!("Run {}: {} ({details})", status.run_id.as_deref().unwrap_or("unknown"), status.state,)
}

fn final_artifact_output(artifact_dir: Option<&str>, summary: &str, remote: bool) -> String {
	let location = artifact_dir.unwrap_or("the remote agent");
	let prefix = if remote { "Remote artifacts validated" } else { "Artifacts validated" };
	format!("{prefix}: {location}\n\n{summary}")
}

fn sample_failure_line(run_id: &str, sample: &crate::runner::SampleBatch) -> String {
	format!(
		"Run {run_id}: sample failed: {} ({}): {}",
		sample.label, sample.response_code, sample.response_message
	)
}

fn run_event_payload(run_id: &str, event: RunEvent) -> Value {
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
	json!({ "version": EVENT_VERSION, "event": event_name, "runId": run_id, "data": value })
}

#[derive(Default)]
struct HumanProgressReporter {
	last_state: Option<String>,
	last_emitted: Option<Instant>,
}

impl HumanProgressReporter {
	fn should_emit(&mut self, status: &RunStatus) -> bool {
		let state_changed = self.last_state.as_deref() != Some(status.state.as_str());
		let due = self.last_emitted.is_none_or(|last| last.elapsed() >= Duration::from_secs(1));
		if state_changed || due {
			self.last_state = Some(status.state.clone());
			self.last_emitted = Some(Instant::now());
			true
		} else {
			false
		}
	}
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
		if self.format == OutputFormat::Human {
			if let RunEvent::Sample(sample) = event {
				if !sample.success {
					eprintln!("{}", sample_failure_line(&self.run_id, &sample));
				}
			}
			return;
		}
		write_json_line(&run_event_payload(&self.run_id, event));
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn command_line_enforces_run_argument_constraints() {
		assert!(Cli::try_parse_from(["polkameter", "validate", "scenario.json"]).is_ok());
		assert!(
			Cli::try_parse_from(["polkameter", "run", "scenario.json", "--output", "runs"]).is_ok()
		);
		assert!(Cli::try_parse_from(["polkameter", "agent", "serve"]).is_ok());
		assert!(Cli::try_parse_from(["polkameter", "run", "scenario.json"]).is_err());
		assert!(Cli::try_parse_from([
			"polkameter",
			"run",
			"scenario.json",
			"--output",
			"runs",
			"--signer-profile",
			"local-dev",
			"--signer-env",
			"POLKAMETER_SURI",
		])
		.is_err());
		assert!(Cli::try_parse_from([
			"polkameter",
			"run",
			"scenario.json",
			"--output",
			"runs",
			"--remote-token-env",
			"POLKAMETER_REMOTE_TOKEN",
		])
		.is_err());
		assert!(Cli::try_parse_from([
			"polkameter",
			"run",
			"scenario.json",
			"--remote",
			"http://127.0.0.1:9901",
			"--remote-token-env",
			"POLKAMETER_REMOTE_TOKEN",
		])
		.is_ok());
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
	fn cli_json_payloads_redact_configured_signer_material() {
		let document = crate::artifacts::test_scenario();
		assert_eq!(document.signer_source.base_suri, "//Alice");
		let validation = validation_payload(&document.validate(), estimated_samples(&document));
		let preflight = preflight_payload(&crate::preflight::PreflightReport {
			run_id: "run-1".into(),
			signer_derivation_root: "//polkameter//run-run-1".into(),
			endpoint: document.chain.endpoint.clone(),
			genesis_hash: "0x01".into(),
			spec_version: 1,
			transaction_version: 1,
			metadata_hash: "0x02".into(),
			pallets: vec![],
			selected_calls: vec![],
			derived_accounts: vec![],
			readiness: crate::preflight::Readiness {
				signer_source: "resolved in memory".into(),
				balance_and_nonce: "ready".into(),
				transaction_profile: "Polkadot".into(),
			},
			resolved_sample_count: estimated_samples(&document),
		});
		let event = run_event_payload(
			"run-1",
			RunEvent::Sample(crate::runner::SampleBatch {
				label: "balances.transfer_keep_alive".into(),
				success: true,
				elapsed_ms: 12,
				response_code: "0".into(),
				response_message: "Finalized".into(),
				completed_samples: 1,
			}),
		);
		let remote_request = serde_json::to_value(crate::remote::RemoteRunRequest {
			protocol_version: crate::remote::PROTOCOL_VERSION,
			run_id: "run-1".into(),
			document: document.redacted_clone(),
		})
		.expect("remote request serializes");
		for payload in [validation, preflight, event, remote_request.clone()] {
			assert!(!payload.to_string().contains("//Alice"));
		}
		assert!(remote_request.to_string().contains("[redacted]"));
	}

	#[test]
	fn human_results_and_progress_use_separate_streams() {
		assert_eq!(
			output_destination(OutputFormat::Human, OutputKind::Result),
			OutputDestination::Stdout
		);
		assert_eq!(
			output_destination(OutputFormat::Human, OutputKind::Progress),
			OutputDestination::Stderr
		);
		assert_eq!(
			output_destination(OutputFormat::Json, OutputKind::Result),
			OutputDestination::Stdout
		);
		assert_eq!(
			output_destination(OutputFormat::Json, OutputKind::Progress),
			OutputDestination::Stdout
		);
	}

	#[test]
	fn human_progress_is_throttled_but_state_changes_are_reported() {
		let mut reporter = HumanProgressReporter::default();
		let mut status = RunStatus { state: "running".into(), ..RunStatus::default() };
		assert!(reporter.should_emit(&status));
		assert!(!reporter.should_emit(&status));
		status.state = "completed".into();
		assert!(reporter.should_emit(&status));
	}

	#[test]
	fn human_sample_failure_names_the_sampler_and_error_without_changing_json() {
		let sample = crate::runner::SampleBatch {
			label: "balances.transfer_keep_alive".into(),
			success: false,
			elapsed_ms: 120,
			response_code: "DISPATCH_ERROR".into(),
			response_message: "InsufficientBalance".into(),
			completed_samples: 3,
		};
		assert_eq!(
			sample_failure_line("run-1", &sample),
			"Run run-1: sample failed: balances.transfer_keep_alive (DISPATCH_ERROR): InsufficientBalance"
		);
		assert!(!serde_json::to_string(&sample)
			.expect("sample serializes")
			.contains("responseMessage"));
	}
}
