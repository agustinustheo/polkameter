mod application;
mod artifacts;
pub mod cli;
mod jmx;
mod preflight;
mod remote;
mod report;
mod runner;
mod scenario;
mod scheduler;
mod subxt_adapter;
mod telemetry;

use serde::{Deserialize, Serialize};
use tauri::{Emitter, Manager};

use scenario::{ArrivalModel as NativeArrivalModel, ScenarioDocument, ValidationIssue};

const SIGNER_KEYRING_SERVICE: &str = "io.github.agustinustheo.polkameter";

struct TauriRunEventSink(tauri::AppHandle);

impl runner::RunEventSink for TauriRunEventSink {
	fn emit(&self, event: runner::RunEvent) {
		match event {
			runner::RunEvent::Status(status) => {
				let _ = self.0.emit("run-status", status);
			},
			runner::RunEvent::Sample(sample) => {
				let _ = self.0.emit("sample-batch", sample);
			},
		}
	}
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct Scenario {
	name: String,
	endpoint: String,
	pallet: String,
	call: String,
	arguments_json: String,
	signer_source: String,
	virtual_users: u32,
	concurrency: u32,
	arrival: ArrivalModel,
	completion: CompletionBoundary,
	mortality_period: u32,
	finality_timeout_ms: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
enum ArrivalModel {
	Burst { window_ms: u64 },
	Ramp { duration_ms: u64 },
	Poisson { rate_per_second: f64 },
}

impl From<&ArrivalModel> for NativeArrivalModel {
	fn from(value: &ArrivalModel) -> Self {
		match value {
			ArrivalModel::Burst { window_ms } => Self::Burst { window_ms: *window_ms },
			ArrivalModel::Ramp { duration_ms } => Self::Ramp { duration_ms: *duration_ms },
			ArrivalModel::Poisson { rate_per_second } => {
				Self::Poisson { rate_per_second: *rate_per_second }
			},
		}
	}
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
enum CompletionBoundary {
	Submitted,
	InBlock,
	Finalized,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ScenarioValidation {
	valid: bool,
	issues: Vec<ValidationIssue>,
	estimated_samples: u32,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct SchedulePreview {
	offsets_ms: Vec<u64>,
	duration_ms: u64,
	batch_size: u32,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct NativeScenarioValidation {
	valid: bool,
	issues: Vec<ValidationIssue>,
	estimated_samples: u64,
}

#[tauri::command]
fn validate_scenario(scenario: Scenario) -> ScenarioValidation {
	let mut issues = Vec::new();
	let endpoint = scenario.endpoint.trim();

	if scenario.name.trim().is_empty() {
		issues.push(ValidationIssue::new("name", "must not be empty"));
	}
	if !(endpoint.starts_with("ws://") || endpoint.starts_with("wss://")) {
		issues.push(ValidationIssue::new("endpoint", "must use ws:// or wss://"));
	}
	if scenario.pallet.trim().is_empty() {
		issues.push(ValidationIssue::new("pallet", "must not be empty"));
	}
	if scenario.call.trim().is_empty() {
		issues.push(ValidationIssue::new("call", "must not be empty"));
	}
	if scenario.arguments_json.trim().is_empty() {
		issues.push(ValidationIssue::new("argumentsJson", "must not be empty"));
	} else if !is_json_object_or_array(&scenario.arguments_json) {
		issues.push(ValidationIssue::new("argumentsJson", "must be a JSON object or array"));
	}
	if scenario.signer_source.trim().is_empty() {
		issues.push(ValidationIssue::new("signerSource", "must not be empty"));
	}
	if scenario.virtual_users == 0 {
		issues.push(ValidationIssue::new("virtualUsers", "must be at least one"));
	}
	if scenario.concurrency == 0 || scenario.concurrency > scenario.virtual_users {
		issues.push(ValidationIssue::new(
			"concurrency",
			"must be between one and the virtual-user count",
		));
	}
	if !scenario.mortality_period.is_power_of_two() || scenario.mortality_period < 4 {
		issues.push(ValidationIssue::new(
			"mortalityPeriod",
			"must be a power of two of at least four",
		));
	}
	if scenario.finality_timeout_ms < 1_000 {
		issues.push(ValidationIssue::new("finalityTimeoutMs", "must be at least 1000 ms"));
	}
	if let Err(message) = NativeArrivalModel::from(&scenario.arrival).validate() {
		issues.push(ValidationIssue::new("arrival", message));
	}

	let _completion = scenario.completion;
	ScenarioValidation {
		valid: issues.is_empty(),
		issues,
		estimated_samples: scenario.virtual_users,
	}
}

#[tauri::command]
fn validate_native_scenario(document: scenario::ScenarioDocument) -> NativeScenarioValidation {
	let issues = document.validate();
	let estimated_samples = document
		.thread_groups
		.iter()
		.map(|group| u64::from(group.users) * group.samplers.len() as u64)
		.sum();
	NativeScenarioValidation { valid: issues.is_empty(), issues, estimated_samples }
}

#[tauri::command]
fn preview_schedule(
	virtual_users: u32,
	arrival: ArrivalModel,
	seed: Option<u64>,
) -> Result<SchedulePreview, String> {
	let arrival = NativeArrivalModel::from(&arrival);
	let offsets_ms = scheduler::offsets(virtual_users, &arrival, seed.unwrap_or(0x2a7d_61e5))?;
	let duration_ms = *offsets_ms.last().unwrap_or(&0);
	Ok(SchedulePreview { offsets_ms, duration_ms, batch_size: virtual_users })
}

#[tauri::command]
fn create_artifact_preview(
	document: scenario::ScenarioDocument,
	output_root: String,
) -> Result<String, String> {
	let issues = document.validate();
	if !issues.is_empty() {
		return Err(format!("scenario is invalid: {}", issues[0].message));
	}
	let run_id = artifacts::new_run_id();
	let mut writer = artifacts::ArtifactWriter::create(
		output_root,
		&document,
		&run_id,
		"Polkameter artifact preview created through the desktop application\n",
	)?;
	writer.flush()?;
	let report = report::write(&writer.directory)?;
	writer.write_summary(&format!(
		"# Polkameter Preview\n\nNo transactions were submitted.\n\n{}",
		report.summary
	))?;
	report::validate(&writer.directory)?;
	Ok(writer.directory.display().to_string())
}

#[tauri::command]
fn export_jmx(document: ScenarioDocument) -> String {
	jmx::export(&document)
}

#[tauri::command]
fn save_jmx(document: ScenarioDocument, path: String) -> Result<(), String> {
	if let Some(parent) = std::path::Path::new(&path).parent() {
		std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
	}
	std::fs::write(path, jmx::export(&document)).map_err(|error| error.to_string())
}

#[tauri::command]
fn import_jmx(xml: String) -> Result<jmx::JmxImportReport, String> {
	jmx::import(&xml)
}

#[tauri::command]
async fn start_remote_run(
	target: remote::RemoteRunnerTarget,
	document: ScenarioDocument,
	run_id: String,
) -> Result<runner::RunStatus, String> {
	application::start_remote_run(&target, document, run_id).await
}

#[tauri::command]
async fn get_remote_run_status(
	target: remote::RemoteRunnerTarget,
	run_id: String,
) -> Result<runner::RunStatus, String> {
	application::remote_run_status(&target, &run_id).await
}

#[tauri::command]
async fn stop_remote_run(
	target: remote::RemoteRunnerTarget,
	run_id: String,
) -> Result<runner::RunStatus, String> {
	application::stop_remote_run(&target, &run_id).await
}

#[tauri::command]
async fn read_remote_run_report(
	target: remote::RemoteRunnerTarget,
	run_id: String,
) -> Result<report::DashboardReport, String> {
	application::read_remote_run_report(&target, &run_id).await
}

#[tauri::command]
async fn preflight_scenario(
	mut document: ScenarioDocument,
	run_id: Option<String>,
) -> Result<preflight::PreflightReport, String> {
	application::resolve_signer_source(&mut document, None, None)?;
	let run_id = run_id.unwrap_or_else(artifacts::new_run_id);
	application::preflight_scenario(&document, &run_id).await
}

#[tauri::command]
fn store_signer_profile(profile: String, suri: String) -> Result<(), String> {
	validate_signer_profile(&profile)?;
	if suri.trim().is_empty() {
		return Err("signer SURI must not be empty".into());
	}
	keyring_entry(&profile)?.set_password(&suri).map_err(|error| {
		format!("could not save signer profile in the OS credential store: {error}")
	})
}

#[tauri::command]
fn remove_signer_profile(profile: String) -> Result<(), String> {
	validate_signer_profile(&profile)?;
	keyring_entry(&profile)?.delete_credential().map_err(|error| {
		format!("could not remove signer profile from the OS credential store: {error}")
	})
}

#[tauri::command]
fn save_scenario(document: scenario::ScenarioDocument, path: String) -> Result<(), String> {
	let encoded =
		serde_json::to_vec_pretty(&document.redacted_clone()).map_err(|error| error.to_string())?;
	if let Some(parent) = std::path::Path::new(&path).parent() {
		std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
	}
	std::fs::write(path, encoded).map_err(|error| error.to_string())
}

#[tauri::command]
fn load_scenario(path: String) -> Result<scenario::ScenarioDocument, String> {
	application::load_scenario_document(path)
}

#[tauri::command]
fn read_run_report(artifact_dir: String) -> Result<report::DashboardReport, String> {
	application::read_run_report(artifact_dir)
}

#[tauri::command]
async fn start_run(
	mut document: ScenarioDocument,
	output_root: String,
	run_id: String,
	app: tauri::AppHandle,
	state: tauri::State<'_, std::sync::Arc<runner::RunnerState>>,
) -> Result<runner::RunStatus, String> {
	application::resolve_signer_source(&mut document, None, None)?;
	application::start_local_run(
		document,
		output_root,
		run_id,
		std::sync::Arc::new(TauriRunEventSink(app)),
		state.inner().clone(),
		"Polkameter run started through the desktop application\n",
	)
	.await
}

fn keyring_entry(profile: &str) -> Result<keyring::Entry, String> {
	keyring::Entry::new(SIGNER_KEYRING_SERVICE, profile)
		.map_err(|error| format!("could not initialize the OS credential entry: {error}"))
}

fn validate_signer_profile(profile: &str) -> Result<(), String> {
	if profile.trim().is_empty() {
		return Err("signer profile must not be empty".into());
	}
	if profile.len() > 128
		|| !profile.chars().all(|character| {
			character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
		}) {
		return Err(
			"signer profile may contain only ASCII letters, digits, hyphens, underscores, and periods"
				.into(),
		);
	}
	Ok(())
}

pub(crate) fn resolve_signer_profile(document: &mut ScenarioDocument) -> Result<(), String> {
	#[cfg(debug_assertions)]
	if let Ok(suri) = std::env::var("POLKAMETER_DEBUG_SURI") {
		if suri.trim().is_empty() {
			return Err("POLKAMETER_DEBUG_SURI must not be empty".into());
		}
		if document.signer_source.funding.is_some() && !suri.trim_start().starts_with("//") {
			return Err(
				"development funding requires POLKAMETER_DEBUG_SURI to begin with //".into()
			);
		}
		document.signer_source.base_suri = suri;
		return Ok(());
	}
	validate_signer_profile(&document.signer_source.profile)?;
	let suri = keyring_entry(&document.signer_source.profile)?
		.get_password()
		.map_err(|error| {
			format!("could not load signer profile '{}': {error}", document.signer_source.profile)
		})?;
	if suri.trim().is_empty() {
		return Err("stored signer SURI must not be empty".into());
	}
	if document.signer_source.funding.is_some() && !suri.trim_start().starts_with("//") {
		return Err(
			"development funding requires a stored development SURI beginning with //".into()
		);
	}
	document.signer_source.base_suri = suri;
	Ok(())
}

#[tauri::command]
async fn stop_run(
	state: tauri::State<'_, std::sync::Arc<runner::RunnerState>>,
) -> Result<runner::RunStatus, String> {
	application::stop_local_run(state.inner().clone()).await
}

#[tauri::command]
async fn get_run_status(
	state: tauri::State<'_, std::sync::Arc<runner::RunnerState>>,
) -> Result<runner::RunStatus, String> {
	Ok(application::run_status(state.inner().clone()).await)
}

fn is_json_object_or_array(value: &str) -> bool {
	matches!(
		serde_json::from_str::<serde_json::Value>(value),
		Ok(serde_json::Value::Object(_) | serde_json::Value::Array(_))
	)
}

pub fn run() {
	tauri::Builder::<tauri::Wry>::default()
		.manage(std::sync::Arc::new(runner::RunnerState::default()))
		.setup(|app: &mut tauri::App<tauri::Wry>| {
			let Some(window) = app.get_webview_window("main") else {
				return Ok(());
			};
			// Keep first launch on macOS's primary coordinate space. Display-relative
			// centering can resurrect a stale virtual-desktop location after monitor changes.
			window.set_position(tauri::Position::Physical(tauri::PhysicalPosition::new(40, 40)))?;
			Ok(())
		})
		.invoke_handler(tauri::generate_handler![
			validate_scenario,
			validate_native_scenario,
			preview_schedule,
			create_artifact_preview,
			export_jmx,
			save_jmx,
			import_jmx,
			start_remote_run,
			get_remote_run_status,
			stop_remote_run,
			read_remote_run_report,
			preflight_scenario,
			store_signer_profile,
			remove_signer_profile,
			save_scenario,
			load_scenario,
			read_run_report,
			start_run,
			stop_run,
			get_run_status
		])
		.run(tauri::generate_context!())
		.expect("error while running Polkameter");
}

#[cfg(test)]
mod tests {
	use super::{
		preview_schedule, remove_signer_profile, resolve_signer_profile, store_signer_profile,
		validate_signer_profile, ArrivalModel,
	};

	#[test]
	fn burst_preview_is_seeded_and_bounded() {
		let preview = preview_schedule(4, ArrivalModel::Burst { window_ms: 1_000 }, Some(8))
			.expect("valid schedule");
		assert_eq!(preview.offsets_ms.len(), 4);
		assert!(preview.offsets_ms.iter().all(|offset| *offset <= 1_000));
	}

	#[test]
	fn arrival_model_accepts_frontend_wire_format() {
		let arrival =
			serde_json::from_str::<ArrivalModel>(r#"{"kind":"poisson","ratePerSecond":20}"#)
				.expect("frontend arrival model");
		assert!(matches!(arrival, ArrivalModel::Poisson { rate_per_second: 20.0 }));
	}

	#[test]
	fn signer_profile_names_are_constrained_before_reaching_the_os_store() {
		assert!(validate_signer_profile("local-dev.1").is_ok());
		assert!(validate_signer_profile("").is_err());
		assert!(validate_signer_profile("contains space").is_err());
	}

	#[test]
	#[ignore = "requires an operating-system credential manager"]
	fn signer_profile_round_trips_through_the_os_credential_manager() {
		let profile = format!("polkameter-test-{}", std::process::id());
		let mut document = crate::artifacts::test_scenario();
		document.signer_source.profile = profile.clone();
		document.signer_source.base_suri.clear();
		let _ = remove_signer_profile(profile.clone());
		let result = (|| -> Result<String, String> {
			store_signer_profile(profile.clone(), "//Alice".into())?;
			resolve_signer_profile(&mut document)?;
			Ok(document.signer_source.base_suri.clone())
		})();
		let cleanup = remove_signer_profile(profile);
		assert_eq!(result.expect("profile resolved"), "//Alice");
		cleanup.expect("profile removed");
	}
}
