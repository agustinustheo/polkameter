use std::{
	sync::Arc,
	time::{Duration, SystemTime, UNIX_EPOCH},
};

use futures::{
	future,
	stream::{self, StreamExt},
};
use serde::{Deserialize, Serialize};
use tokio::sync::{watch, Mutex, Semaphore};

use crate::{
	artifacts::{new_run_id, ArtifactWriter, EventRecord, SampleRecord},
	scenario::{
		signer_offset, CompletionBoundary, SamplerPhase, ScenarioDocument, ThreadGroup,
		TransactionProfile, TransactionSampler,
	},
	scheduler,
	subxt_adapter::{Submission, SubxtRuntimeAdapter},
};

#[derive(Default)]
pub struct RunnerState {
	cancel: Mutex<Option<watch::Sender<bool>>>,
	status: Mutex<RunStatus>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunStatus {
	pub state: String,
	pub run_id: Option<String>,
	pub artifact_dir: Option<String>,
	pub completed_samples: u64,
	pub successful_samples: u64,
	pub failed_samples: u64,
	pub timed_out_samples: u64,
	pub message: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SampleBatch {
	label: String,
	success: bool,
	elapsed_ms: u64,
	response_code: String,
	completed_samples: u64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RunEvent {
	Status(RunStatus),
	Sample(SampleBatch),
}

pub trait RunEventSink: Send + Sync {
	fn emit(&self, event: RunEvent);
}

const HEADLESS_COMMAND: &str = "Polkameter run started through a headless runner\n";

pub async fn start_with_command(
	document: ScenarioDocument,
	output_root: String,
	run_id: String,
	sink: Arc<dyn RunEventSink>,
	state: Arc<RunnerState>,
	command: impl Into<String>,
) -> Result<RunStatus, String> {
	if !document.validate().is_empty() {
		return Err("scenario must pass structural validation before it can be armed".into());
	}
	if !matches!(document.chain.transaction_profile, TransactionProfile::Polkadot) {
		return Err("this build can execute the standard Polkadot transaction profile only".into());
	}
	let mut current = state.status.lock().await;
	if current.state == "running" || current.state == "arming" {
		return Err("a run is already active".into());
	}
	let command = command.into();
	let writer = ArtifactWriter::create(&output_root, &document, &run_id, &command)?;
	let (cancel, cancel_rx) = watch::channel(false);
	*state.cancel.lock().await = Some(cancel);
	*current = RunStatus {
		state: "arming".into(),
		run_id: Some(run_id.clone()),
		artifact_dir: Some(writer.directory.display().to_string()),
		message: Some("validating live signer readiness".into()),
		..RunStatus::default()
	};
	let status = current.clone();
	drop(current);
	emit_status(Some(sink.as_ref()), &status);

	tokio::spawn(async move {
		let result =
			execute(document, writer, run_id, Some(sink.clone()), state.clone(), cancel_rx).await;
		let mut final_status = state.status.lock().await;
		match result {
			Ok(summary) => *final_status = summary,
			Err(error) => {
				final_status.state = "failed".into();
				final_status.message = Some(error);
			},
		}
		emit_status(Some(sink.as_ref()), &final_status);
		*state.cancel.lock().await = None;
	});

	Ok(status)
}

async fn run_group_phase(
	runtime: Arc<SubxtRuntimeAdapter>,
	document: ScenarioDocument,
	group: ThreadGroup,
	group_position: usize,
	phase: SamplerPhase,
	run_id: String,
	cancel: watch::Receiver<bool>,
	deadline: tokio::time::Instant,
	global_sample_gate: Option<Arc<Semaphore>>,
) -> Result<Vec<TaskResult>, String> {
	let samplers = group
		.samplers
		.iter()
		.filter(|sampler| same_phase(&sampler.phase, &phase))
		.cloned()
		.collect::<Vec<_>>();
	if samplers.is_empty() {
		return Ok(Vec::new());
	}
	let signer_offset = signer_offset(&document, group_position);
	let scheduled_users = if matches!(phase, SamplerPhase::Transaction) {
		scheduler::offsets(group.users, &group.arrival, document.test_plan.seed)?
			.into_iter()
			.enumerate()
			.map(|(index, offset)| (index as u32, offset))
			.collect::<Vec<_>>()
	} else {
		vec![(0, 0)]
	};
	let phase_concurrency =
		if matches!(phase, SamplerPhase::Transaction) { group.concurrency as usize } else { 1 };
	let iterations = if matches!(phase, SamplerPhase::Transaction) { group.iterations } else { 1 };
	Ok(stream::iter(scheduled_users)
		.map(|(index, offset_ms)| {
			let runtime = runtime.clone();
			let document = document.clone();
			let group_name = group.name.clone();
			let samplers = samplers.clone();
			let cancel = cancel.clone();
			let run_id = run_id.clone();
			let global_sample_gate = global_sample_gate.clone();
			let iterations = iterations;
			async move {
				let signer_index = signer_offset + index;
				let cancelled = tokio::select! {
					cancelled = wait_or_cancel(Duration::from_millis(offset_ms), cancel.clone()) => cancelled,
					_ = tokio::time::sleep_until(deadline) => return TaskResult::run_timeout(signer_index, group_name),
				};
				if cancelled {
					return TaskResult::cancelled(signer_index, group_name);
				}
				let _permit = if let Some(gate) = global_sample_gate {
					let mut permit_cancel = cancel.clone();
					Some(tokio::select! {
						permit = gate.acquire_owned() => match permit {
							Ok(permit) => permit,
							Err(_) => return TaskResult::run_timeout(signer_index, group_name),
						},
						_ = permit_cancel.changed() => return TaskResult::cancelled(signer_index, group_name),
						_ = tokio::time::sleep_until(deadline) => return TaskResult::run_timeout(signer_index, group_name),
					})
				} else {
					None
				};
				match tokio::time::timeout(
					deadline.saturating_duration_since(tokio::time::Instant::now()),
					execute_user(
						&runtime,
						&document,
						&group_name,
						signer_index,
						&samplers,
						iterations,
						&run_id,
						cancel,
						Duration::from_millis(document.test_plan.limits.shutdown_drain_timeout_ms),
					),
				)
				.await
				{
					Ok(result) => result,
					Err(_) => TaskResult::run_timeout(signer_index, group_name),
				}
			}
		})
		.buffer_unordered(phase_concurrency)
		.collect::<Vec<_>>()
		.await)
}

#[allow(dead_code)]
pub async fn run_headless(
	document: ScenarioDocument,
	output_root: String,
) -> Result<RunStatus, String> {
	if !document.validate().is_empty() {
		return Err("scenario must pass structural validation before it can be armed".into());
	}
	if !matches!(document.chain.transaction_profile, TransactionProfile::Polkadot) {
		return Err("this build can execute the standard Polkadot transaction profile only".into());
	}
	let run_id = new_run_id();
	run_headless_with_run_id(document, output_root, run_id).await
}

#[allow(dead_code)]
pub async fn run_headless_with_run_id(
	document: ScenarioDocument,
	output_root: String,
	run_id: String,
) -> Result<RunStatus, String> {
	if !document.validate().is_empty() {
		return Err("scenario must pass structural validation before it can be armed".into());
	}
	if !matches!(document.chain.transaction_profile, TransactionProfile::Polkadot) {
		return Err("this build can execute the standard Polkadot transaction profile only".into());
	}
	let writer = ArtifactWriter::create(output_root, &document, &run_id, HEADLESS_COMMAND)?;
	let (_cancel, cancel_rx) = watch::channel(false);
	execute(document, writer, run_id, None, Arc::new(RunnerState::default()), cancel_rx).await
}

pub async fn stop(state: Arc<RunnerState>) -> Result<RunStatus, String> {
	let cancel = state.cancel.lock().await.clone().ok_or("no active run")?;
	cancel.send(true).map_err(|_| "run was already stopping")?;
	let mut status = state.status.lock().await;
	status.state = "stopping".into();
	status.message =
		Some("graceful stop requested; active submissions will finish or time out".into());
	Ok(status.clone())
}

pub async fn status(state: Arc<RunnerState>) -> RunStatus {
	state.status.lock().await.clone()
}

async fn execute(
	document: ScenarioDocument,
	mut writer: ArtifactWriter,
	run_id: String,
	sink: Option<Arc<dyn RunEventSink>>,
	state: Arc<RunnerState>,
	cancel: watch::Receiver<bool>,
) -> Result<RunStatus, String> {
	let mut counts = RunCounts::default();
	let runtime = match SubxtRuntimeAdapter::connect(&document.chain.endpoint).await {
		Ok(runtime) => Arc::new(runtime),
		Err(error) => return failed_artifact(writer, &counts, error),
	};
	if let Err(error) = runtime.fund_derived_signers(&document, &run_id).await {
		return failed_artifact(writer, &counts, error);
	}
	if let Err(error) = runtime.ensure_ready(&document, &run_id).await {
		return failed_artifact(writer, &counts, error);
	}
	{
		let mut status = state.status.lock().await;
		status.state = "running".into();
		status.message = Some("executing thread groups".into());
		emit_status(sink.as_deref(), &status);
	}
	let started = now_ms();
	let deadline = tokio::time::Instant::now()
		+ Duration::from_millis(document.test_plan.limits.whole_run_timeout_ms);
	let telemetry = crate::telemetry::spawn(
		&writer.directory,
		document.chain.endpoint.clone(),
		document.chain.prometheus_endpoint.clone(),
		started,
		state.clone(),
	)?;
	let global_sample_gate =
		Arc::new(Semaphore::new(document.test_plan.limits.max_concurrent_samples as usize));

	for phase in [SamplerPhase::Setup, SamplerPhase::Transaction, SamplerPhase::Teardown] {
		let group_results = if matches!(phase, SamplerPhase::Transaction) {
			future::try_join_all(document.thread_groups.iter().cloned().enumerate().map(
				|(group_position, group)| {
					run_group_phase(
						runtime.clone(),
						document.clone(),
						group,
						group_position,
						phase,
						run_id.clone(),
						cancel.clone(),
						deadline,
						Some(global_sample_gate.clone()),
					)
				},
			))
			.await?
		} else {
			let mut results = Vec::new();
			for (group_position, group) in document.thread_groups.iter().cloned().enumerate() {
				results.push(
					run_group_phase(
						runtime.clone(),
						document.clone(),
						group,
						group_position,
						phase,
						run_id.clone(),
						cancel.clone(),
						deadline,
						None,
					)
					.await?,
				);
				if *cancel.borrow() {
					break;
				}
			}
			results
		};
		for group_result in group_results {
			for task in group_result {
				for result in task.samples {
					if let Err(error) = record_result(
						&mut writer,
						sink.as_deref(),
						&state,
						started,
						&mut counts,
						result,
					)
					.await
					{
						let _ = telemetry.stop().await;
						return failed_artifact(writer, &counts, error);
					}
				}
			}
		}
		if *cancel.borrow() {
			break;
		}
	}

	let telemetry_error = telemetry.stop().await.err();
	writer.flush()?;
	let report = crate::report::write(&writer.directory)?;
	writer.write_summary(&report.summary)?;
	crate::report::validate(&writer.directory)?;
	if let Some(error) = telemetry_error {
		return failed_artifact(writer, &counts, format!("telemetry collector failed: {error}"));
	}
	let mut status = RunStatus {
		state: if *cancel.borrow() {
			"stopped".into()
		} else if counts.failed_samples == 0 {
			"completed".into()
		} else {
			"completed_with_failures".into()
		},
		run_id: Some(
			writer.directory.file_name().unwrap_or_default().to_string_lossy().to_string(),
		),
		artifact_dir: Some(writer.directory.display().to_string()),
		completed_samples: counts.completed_samples,
		successful_samples: counts.successful_samples,
		failed_samples: counts.failed_samples,
		timed_out_samples: counts.timed_out_samples,
		message: Some("artifacts written".into()),
	};
	if status.state == "completed_with_failures" {
		status.message = Some("artifacts written with failed samples".into());
	}
	Ok(status)
}

fn failed_artifact(
	mut writer: ArtifactWriter,
	counts: &RunCounts,
	error: String,
) -> Result<RunStatus, String> {
	writer.flush()?;
	let report = crate::report::write(&writer.directory)?;
	writer.write_summary(&format!(
		"# Polkameter Run\n\n## Execution failure\n\n{}\n\n{}",
		error, report.summary
	))?;
	crate::report::validate(&writer.directory)?;
	Ok(RunStatus {
		state: "failed".into(),
		run_id: Some(
			writer.directory.file_name().unwrap_or_default().to_string_lossy().to_string(),
		),
		artifact_dir: Some(writer.directory.display().to_string()),
		completed_samples: counts.completed_samples,
		successful_samples: counts.successful_samples,
		failed_samples: counts.failed_samples,
		timed_out_samples: counts.timed_out_samples,
		message: Some(format!("artifacts written after failure: {error}")),
	})
}

async fn execute_user(
	runtime: &SubxtRuntimeAdapter,
	document: &ScenarioDocument,
	group_name: &str,
	index: u32,
	samplers: &[TransactionSampler],
	iterations: u32,
	run_id: &str,
	cancel: watch::Receiver<bool>,
	shutdown_drain_timeout: Duration,
) -> TaskResult {
	let mut samples = Vec::new();
	for _ in 0..iterations {
		for sampler in samplers {
			if *cancel.borrow() {
				samples.push(TaskSample::aborted(group_name, index, sampler));
				return TaskResult { samples };
			}
			let start = now_ms();
			let submission = runtime.submit(document, index, run_id, sampler);
			tokio::pin!(submission);
			let mut cancellation = cancel.clone();
			let result = tokio::select! {
				result = &mut submission => Some(result),
				_ = cancellation.changed() => tokio::time::timeout(shutdown_drain_timeout, &mut submission).await.ok(),
			};
			let end = now_ms();
			samples.push(match result {
				None => TaskSample::aborted_after_stop(group_name, index, sampler, start, end),
				Some(Ok(submission)) => {
					assertion_result(group_name, index, sampler, start, end, submission)
				},
				Some(Err(error)) => {
					TaskSample::failure(group_name, index, sampler, start, end, error)
				},
			});
			if *cancel.borrow() {
				return TaskResult { samples };
			}
		}
	}
	TaskResult { samples }
}

async fn record_result(
	writer: &mut ArtifactWriter,
	sink: Option<&dyn RunEventSink>,
	state: &Arc<RunnerState>,
	started: u64,
	counts: &mut RunCounts,
	result: TaskSample,
) -> Result<(), String> {
	counts.completed_samples += 1;
	if result.success {
		counts.successful_samples += 1;
	} else {
		counts.failed_samples += 1;
	}
	if result.response_code.contains("TIMEOUT") {
		counts.timed_out_samples += 1;
	}
	{
		let mut status = state.status.lock().await;
		status.completed_samples = counts.completed_samples;
		status.successful_samples = counts.successful_samples;
		status.failed_samples = counts.failed_samples;
		status.timed_out_samples = counts.timed_out_samples;
	}
	writer.write_sample(&SampleRecord {
		timestamp: result.start_ms,
		elapsed: result.end_ms.saturating_sub(result.start_ms),
		label: result.label.clone(),
		response_code: result.response_code.clone(),
		response_message: result.message.clone(),
		thread_name: result.thread_name.clone(),
		success: result.success,
		bytes: 0,
		sent_bytes: 0,
		latency: result.end_ms.saturating_sub(result.start_ms),
		connect: 0,
		all_threads: 0,
		group_threads: 0,
	})?;
	writer.write_event(&EventRecord {
		ts: result.start_ms.to_string(),
		label: result.label.clone(),
		account: result.account,
		sampler_phase: result.phase,
		scheduled_ms: result.start_ms.saturating_sub(started),
		submit_ms: Some(result.start_ms),
		completed_ms: Some(result.end_ms),
		extrinsic_hash: result.extrinsic_hash.clone(),
		block_hash: result.block_hash.clone(),
		outcome: if result.success { "success".into() } else { "failure".into() },
		message: result.message.clone(),
	})?;
	if let Some(sink) = sink {
		sink.emit(RunEvent::Sample(SampleBatch {
			label: result.label,
			success: result.success,
			elapsed_ms: result.end_ms.saturating_sub(result.start_ms),
			response_code: result.response_code,
			completed_samples: counts.completed_samples,
		}));
	}
	Ok(())
}

async fn wait_or_cancel(duration: Duration, mut cancel: watch::Receiver<bool>) -> bool {
	tokio::select! {
		_ = tokio::time::sleep(duration) => false,
		_ = cancel.changed() => true,
	}
}

fn same_phase(left: &SamplerPhase, right: &SamplerPhase) -> bool {
	matches!(
		(left, right),
		(SamplerPhase::Setup, SamplerPhase::Setup)
			| (SamplerPhase::Transaction, SamplerPhase::Transaction)
			| (SamplerPhase::Teardown, SamplerPhase::Teardown)
	)
}

fn emit_status(sink: Option<&dyn RunEventSink>, status: &RunStatus) {
	if let Some(sink) = sink {
		sink.emit(RunEvent::Status(status.clone()));
	}
}

fn now_ms() -> u64 {
	SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.unwrap_or_default()
		.as_millis()
		.try_into()
		.unwrap_or(u64::MAX)
}

#[derive(Default)]
struct RunCounts {
	completed_samples: u64,
	successful_samples: u64,
	failed_samples: u64,
	timed_out_samples: u64,
}

struct TaskResult {
	samples: Vec<TaskSample>,
}
impl TaskResult {
	fn cancelled(index: u32, group: String) -> Self {
		Self { samples: vec![TaskSample::aborted(&group, index, &abort_sampler())] }
	}
	fn run_timeout(index: u32, group: String) -> Self {
		Self { samples: vec![TaskSample::run_timeout(&group, index)] }
	}
}

struct TaskSample {
	label: String,
	thread_name: String,
	account: String,
	phase: String,
	start_ms: u64,
	end_ms: u64,
	success: bool,
	response_code: String,
	message: String,
	extrinsic_hash: Option<String>,
	block_hash: Option<String>,
}

impl TaskSample {
	fn success(
		group: &str,
		index: u32,
		sampler: &TransactionSampler,
		start_ms: u64,
		end_ms: u64,
		submission: Submission,
	) -> Self {
		Self {
			label: sampler.label.clone(),
			thread_name: group.into(),
			account: index.to_string(),
			phase: format!("{:?}", sampler.phase).to_lowercase(),
			start_ms,
			end_ms,
			success: true,
			response_code: "OK".into(),
			message: submission.message,
			extrinsic_hash: submission.extrinsic_hash,
			block_hash: submission.block_hash,
		}
	}
	fn failure(
		group: &str,
		index: u32,
		sampler: &TransactionSampler,
		start_ms: u64,
		end_ms: u64,
		message: String,
	) -> Self {
		Self {
			label: sampler.label.clone(),
			thread_name: group.into(),
			account: index.to_string(),
			phase: format!("{:?}", sampler.phase).to_lowercase(),
			start_ms,
			end_ms,
			success: false,
			response_code: failure_code(&message).into(),
			message,
			extrinsic_hash: None,
			block_hash: None,
		}
	}
	fn aborted(group: &str, index: u32, sampler: &TransactionSampler) -> Self {
		Self {
			label: sampler.label.clone(),
			thread_name: group.into(),
			account: index.to_string(),
			phase: format!("{:?}", sampler.phase).to_lowercase(),
			start_ms: now_ms(),
			end_ms: now_ms(),
			success: false,
			response_code: "ABORTED".into(),
			message: "run stopped before this sample began".into(),
			extrinsic_hash: None,
			block_hash: None,
		}
	}
	fn aborted_after_stop(
		group: &str,
		index: u32,
		sampler: &TransactionSampler,
		start_ms: u64,
		end_ms: u64,
	) -> Self {
		Self {
			label: sampler.label.clone(),
			thread_name: group.into(),
			account: index.to_string(),
			phase: format!("{:?}", sampler.phase).to_lowercase(),
			start_ms,
			end_ms,
			success: false,
			response_code: "ABORTED".into(),
			message: "shutdown drain deadline elapsed while the active sample was still pending"
				.into(),
			extrinsic_hash: None,
			block_hash: None,
		}
	}
	fn run_timeout(group: &str, index: u32) -> Self {
		Self {
			label: "run.timeout".into(),
			thread_name: group.into(),
			account: index.to_string(),
			phase: "transaction".into(),
			start_ms: now_ms(),
			end_ms: now_ms(),
			success: false,
			response_code: "RUN_TIMEOUT".into(),
			message: "whole-run deadline elapsed before this user completed".into(),
			extrinsic_hash: None,
			block_hash: None,
		}
	}
}

fn assertion_result(
	group: &str,
	index: u32,
	sampler: &TransactionSampler,
	start_ms: u64,
	end_ms: u64,
	submission: Submission,
) -> TaskSample {
	if let Some(limit) = sampler.assertions.iter().find_map(|assertion| match assertion {
		crate::scenario::Assertion::MaxElapsed { milliseconds } => Some(*milliseconds),
		crate::scenario::Assertion::Success => None,
	}) {
		if end_ms.saturating_sub(start_ms) > limit {
			return TaskSample {
				label: sampler.label.clone(),
				thread_name: group.into(),
				account: index.to_string(),
				phase: format!("{:?}", sampler.phase).to_lowercase(),
				start_ms,
				end_ms,
				success: false,
				response_code: "ASSERTION_FAILED".into(),
				message: format!(
					"elapsed {} ms exceeded assertion limit {limit} ms",
					end_ms.saturating_sub(start_ms)
				),
				extrinsic_hash: submission.extrinsic_hash,
				block_hash: submission.block_hash,
			};
		}
	}
	TaskSample::success(group, index, sampler, start_ms, end_ms, submission)
}

fn failure_code(message: &str) -> &'static str {
	if message == "FINALITY_TIMEOUT" {
		"FINALITY_TIMEOUT"
	} else if message.contains("timeout") {
		"TASK_TIMEOUT"
	} else if message.contains("connect") || message.contains("RPC") || message.contains("rpc") {
		"RPC_ERROR"
	} else {
		"TX_ERROR"
	}
}

fn abort_sampler() -> TransactionSampler {
	TransactionSampler {
		phase: SamplerPhase::Transaction,
		label: "run.aborted".into(),
		pallet: String::new(),
		call: String::new(),
		arguments: serde_json::json!({}),
		completion: CompletionBoundary::Submitted,
		mortality_period: 4,
		finality_timeout_ms: 1_000,
		assertions: vec![],
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[derive(Default)]
	struct RecordingSink(std::sync::Mutex<Vec<RunEvent>>);

	impl RunEventSink for RecordingSink {
		fn emit(&self, event: RunEvent) {
			self.0.lock().expect("event sink lock").push(event);
		}
	}

	#[tokio::test]
	async fn scheduled_wait_stops_when_cancellation_arrives() {
		let (stop, receiver) = watch::channel(false);
		stop.send(true).expect("receiver alive");
		assert!(wait_or_cancel(Duration::from_secs(60), receiver).await);
	}

	#[test]
	fn sampler_phases_are_kept_separate() {
		assert!(same_phase(&SamplerPhase::Setup, &SamplerPhase::Setup));
		assert!(!same_phase(&SamplerPhase::Setup, &SamplerPhase::Transaction));
	}

	#[test]
	fn failure_codes_keep_timeouts_and_rpc_failures_distinct() {
		assert_eq!(failure_code("FINALITY_TIMEOUT"), "FINALITY_TIMEOUT");
		assert_eq!(failure_code("could not connect to RPC"), "RPC_ERROR");
		assert_eq!(failure_code("dispatch failed"), "TX_ERROR");
	}

	#[test]
	fn whole_run_timeout_has_a_distinct_sample_code() {
		let sample = TaskResult::run_timeout(3, "users".into()).samples.pop().expect("sample");
		assert_eq!(sample.response_code, "RUN_TIMEOUT");
		assert!(!sample.success);
	}

	#[test]
	fn derived_signers_are_scoped_to_the_run_root() {
		let document = crate::artifacts::test_scenario();
		assert_ne!(
			crate::scenario::signer_suri(&document, 1, "run-a"),
			crate::scenario::signer_suri(&document, 1, "run-b")
		);
		let mut base_only = document.clone();
		base_only.signer_source.derivation_path.clear();
		assert_eq!(crate::scenario::signer_suri(&base_only, 0, "run-a"), "//Alice");
	}

	#[test]
	fn readiness_covers_every_thread_group_signer() {
		let mut document = crate::artifacts::test_scenario();
		let mut second = document.thread_groups[0].clone();
		second.users = 7;
		document.thread_groups.push(second);
		assert_eq!(crate::scenario::required_signer_count(&document), 8);
		assert_eq!(signer_offset(&document, 1), 1);
	}

	#[tokio::test]
	#[ignore = "requires a fresh local Polkadot dev node at POLKAMETER_E2E_RPC"]
	async fn fresh_dev_chain_run_writes_validated_artifacts() {
		let total_users = e2e_u32("POLKAMETER_E2E_USERS", 5);
		assert!(total_users >= 2, "POLKAMETER_E2E_USERS must be at least two");
		let iterations = e2e_u32("POLKAMETER_E2E_ITERATIONS", 2);
		let requested_concurrency = e2e_u32("POLKAMETER_E2E_CONCURRENCY", 2);
		let max_concurrent_samples = e2e_u32("POLKAMETER_E2E_MAX_CONCURRENT_SAMPLES", 2);
		let funding_batch_size = e2e_u32("POLKAMETER_E2E_FUNDING_BATCH_SIZE", 2);
		assert!(funding_batch_size <= 100, "POLKAMETER_E2E_FUNDING_BATCH_SIZE must not exceed 100");
		let test_timeout_secs = e2e_u64("POLKAMETER_E2E_TEST_TIMEOUT_SECS", 240);
		let endpoint =
			std::env::var("POLKAMETER_E2E_RPC").unwrap_or_else(|_| "ws://127.0.0.1:9944".into());
		let retained_output = std::env::var_os("POLKAMETER_E2E_OUTPUT_ROOT");
		let root = retained_output.as_ref().map_or_else(
			|| std::env::temp_dir().join(format!("polkameter-e2e-{}", new_run_id())),
			std::path::PathBuf::from,
		);
		let mut document = crate::artifacts::test_scenario();
		document.chain.endpoint = endpoint;
		document.chain.prometheus_endpoint = Some(
			std::env::var("POLKAMETER_E2E_PROMETHEUS")
				.unwrap_or_else(|_| "http://127.0.0.1:9615/metrics".into()),
		);
		document.signer_source.funding = Some(crate::scenario::DevelopmentFunding {
			amount: "10000000000000".into(),
			finality_timeout_ms: 30_000,
			batch_size: funding_batch_size,
		});
		let primary_users = total_users.div_ceil(2);
		let secondary_users = total_users - primary_users;
		document.thread_groups[0].users = primary_users;
		document.thread_groups[0].concurrency = requested_concurrency.min(primary_users);
		document.thread_groups[0].iterations = iterations;
		document.test_plan.limits.max_concurrent_samples = max_concurrent_samples;
		let mut second_group = document.thread_groups[0].clone();
		second_group.name = "secondary users".into();
		second_group.users = secondary_users;
		second_group.concurrency = requested_concurrency.min(secondary_users);
		document.thread_groups.push(second_group);
		let arguments = serde_json::json!({
			"dest": {
				"$variant": "Id",
				"value": { "$bytes": "0x8eaf04151687736326c9fea17e25fc5287613693c912909cb226aa4794f26a48" }
			},
			"value": "1000000000000"
		});
		for group in &mut document.thread_groups {
			let sampler = &mut group.samplers[0];
			sampler.arguments = arguments.clone();
			sampler.finality_timeout_ms = 30_000;
		}
		std::fs::create_dir_all(&root).expect("output directory created");
		let saved = root.join("fresh-dev.polkameter.json");
		std::fs::write(
			&saved,
			serde_json::to_vec_pretty(&document.redacted_clone()).expect("scenario serializable"),
		)
		.expect("redacted scenario saved");
		let mut reopened = serde_json::from_slice::<ScenarioDocument>(
			&std::fs::read(&saved).expect("scenario readable"),
		)
		.expect("scenario decoded")
		.migrate()
		.expect("scenario migrated");
		assert_eq!(reopened.signer_source.base_suri, "[redacted]");
		reopened.signer_source.base_suri = "//Alice".into();
		let run_id = new_run_id();
		let preflight = crate::preflight::preflight(&reopened, &run_id)
			.await
			.expect("metadata preflight");
		assert!(
			preflight.selected_calls.iter().all(|call| call.encodable),
			"metadata preflight rejected the fixture call: {:#?}",
			preflight.selected_calls
		);

		let state = std::sync::Arc::new(RunnerState::default());
		let sink = std::sync::Arc::new(RecordingSink::default());
		let arming = start_with_command(
			reopened,
			root.display().to_string(),
			run_id,
			sink.clone(),
			state.clone(),
			"Polkameter integration test\n",
		)
		.await
		.expect("run arms");
		assert_eq!(arming.state, "arming");
		let final_status = tokio::time::timeout(Duration::from_secs(test_timeout_secs), async {
			loop {
				let current = status(state.clone()).await;
				if matches!(
					current.state.as_str(),
					"completed" | "completed_with_failures" | "failed"
				) {
					return current;
				}
				tokio::time::sleep(Duration::from_millis(100)).await;
			}
		})
		.await
		.expect("run completes before the test deadline");
		assert_eq!(final_status.state, "completed");
		assert_eq!(final_status.failed_samples, 0);
		assert_eq!(final_status.successful_samples, u64::from(total_users) * u64::from(iterations));
		let telemetry = std::fs::read_to_string(
			final_status.artifact_dir.as_ref().expect("artifact directory").to_owned()
				+ "/telemetry.jsonl",
		)
		.expect("telemetry readable");
		assert!(
			telemetry.contains("\"node_ready_transactions\":0"),
			"Prometheus node telemetry was not recorded"
		);
		assert!(sink
			.0
			.lock()
			.expect("event sink lock")
			.iter()
			.any(|event| matches!(event, RunEvent::Sample(sample) if sample.success)));
		let run_dir =
			std::path::PathBuf::from(final_status.artifact_dir.expect("artifact directory"));
		crate::report::validate(&run_dir).expect("artifact bundle validates");
		let samples =
			std::fs::read_to_string(run_dir.join("samples.jtl")).expect("samples readable");
		assert!(samples.contains(",true,"));
		if retained_output.is_none() {
			let _ = std::fs::remove_dir_all(root);
		}
	}

	fn e2e_u32(name: &str, default: u32) -> u32 {
		std::env::var(name).ok().map_or(default, |value| {
			let parsed =
				value.parse().unwrap_or_else(|_| panic!("{name} must be a positive integer"));
			assert!(parsed > 0, "{name} must be a positive integer");
			parsed
		})
	}

	fn e2e_u64(name: &str, default: u64) -> u64 {
		std::env::var(name).ok().map_or(default, |value| {
			let parsed =
				value.parse().unwrap_or_else(|_| panic!("{name} must be a positive integer"));
			assert!(parsed > 0, "{name} must be a positive integer");
			parsed
		})
	}
}
