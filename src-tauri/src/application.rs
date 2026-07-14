use std::{path::Path, sync::Arc};

pub use crate::{
	preflight::PreflightReport,
	remote::RemoteRunnerTarget,
	report::DashboardReport,
	runner::{RunEvent, RunEventSink, RunStatus, RunnerState},
	scenario::ScenarioDocument,
};

pub fn load_scenario_document(path: impl AsRef<Path>) -> Result<ScenarioDocument, String> {
	let encoded = std::fs::read(path).map_err(|error| error.to_string())?;
	let document = serde_json::from_slice::<ScenarioDocument>(&encoded)
		.map_err(|error| error.to_string())?
		.migrate()?;
	let stored_suri = document.signer_source.base_suri.trim();
	if !stored_suri.is_empty() && stored_suri != "[redacted]" {
		return Err(
			"scenario files must not contain a signer SURI; use a signer profile or --signer-env"
				.into(),
		);
	}
	Ok(document)
}

pub fn resolve_signer_source(
	document: &mut ScenarioDocument,
	profile_override: Option<&str>,
	signer_env: Option<&str>,
) -> Result<(), String> {
	if profile_override.is_some() && signer_env.is_some() {
		return Err("--signer-profile and --signer-env cannot be used together".into());
	}
	if let Some(profile) = profile_override {
		crate::validate_signer_profile(profile)?;
		document.signer_source.profile = profile.into();
	}
	if let Some(variable) = signer_env {
		validate_environment_variable_name(variable)?;
		let suri = std::env::var(variable)
			.map_err(|_| format!("signer environment variable {variable} is not set"))?;
		validate_resolved_suri(document, &suri, variable)?;
		document.signer_source.base_suri = suri;
		return Ok(());
	}
	crate::resolve_signer_profile(document)
}

pub async fn preflight_scenario(
	document: &ScenarioDocument,
	run_id: &str,
) -> Result<PreflightReport, String> {
	crate::preflight::preflight(document, run_id).await
}

pub async fn start_local_run(
	document: ScenarioDocument,
	output_root: String,
	run_id: String,
	sink: Arc<dyn RunEventSink>,
	state: Arc<RunnerState>,
	command: &str,
) -> Result<RunStatus, String> {
	crate::runner::start_with_command(document, output_root, run_id, sink, state, command).await
}

pub async fn run_status(state: Arc<RunnerState>) -> RunStatus {
	crate::runner::status(state).await
}

pub async fn stop_local_run(state: Arc<RunnerState>) -> Result<RunStatus, String> {
	crate::runner::stop(state).await
}

pub fn read_run_report(path: impl AsRef<Path>) -> Result<DashboardReport, String> {
	crate::report::read_dashboard(path.as_ref())
}

pub async fn start_remote_run(
	target: &RemoteRunnerTarget,
	document: ScenarioDocument,
	run_id: String,
) -> Result<RunStatus, String> {
	crate::remote::start(target, document, run_id).await
}

pub async fn preflight_remote_run(
	target: &RemoteRunnerTarget,
	document: ScenarioDocument,
	run_id: String,
) -> Result<PreflightReport, String> {
	crate::remote::preflight(target, document, run_id).await
}

pub async fn remote_run_status(
	target: &RemoteRunnerTarget,
	run_id: &str,
) -> Result<RunStatus, String> {
	crate::remote::status(target, run_id).await
}

pub async fn stop_remote_run(
	target: &RemoteRunnerTarget,
	run_id: &str,
) -> Result<RunStatus, String> {
	crate::remote::stop(target, run_id).await
}

pub async fn read_remote_run_report(
	target: &RemoteRunnerTarget,
	run_id: &str,
) -> Result<DashboardReport, String> {
	crate::remote::read_remote_report(target, run_id).await
}

pub async fn serve_agent(
	bind: &str,
	bearer_token: String,
	output_root: String,
) -> Result<(), String> {
	crate::remote::serve(bind, bearer_token, output_root).await
}

pub fn validate_environment_variable_name(value: &str) -> Result<(), String> {
	if value.is_empty()
		|| value.len() > 128
		|| !value.chars().enumerate().all(|(index, character)| {
			character == '_'
				|| character.is_ascii_alphabetic()
				|| (index > 0 && character.is_ascii_digit())
		}) {
		return Err("environment variable names may contain ASCII letters, digits, and underscores, and may not begin with a digit".into());
	}
	Ok(())
}

fn validate_resolved_suri(
	document: &ScenarioDocument,
	suri: &str,
	source: &str,
) -> Result<(), String> {
	if suri.trim().is_empty() {
		return Err(format!("signer source {source} must not be empty"));
	}
	if document.signer_source.funding.is_some() && !suri.trim_start().starts_with("//") {
		return Err(format!(
			"development funding requires {source} to contain a development SURI beginning with //"
		));
	}
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn signer_environment_variable_names_are_constrained() {
		assert!(validate_environment_variable_name("POLKAMETER_SURI").is_ok());
		assert!(validate_environment_variable_name("1SURI").is_err());
		assert!(validate_environment_variable_name("SURI-NAME").is_err());
	}

	#[test]
	fn stored_scenarios_reject_literal_signer_material() {
		let path = std::env::temp_dir()
			.join(format!("polkameter-scenario-secret-test-{}.json", std::process::id()));
		let mut document = crate::artifacts::test_scenario();
		document.signer_source.base_suri = "//Alice".into();
		std::fs::write(&path, serde_json::to_vec(&document).expect("scenario encodes"))
			.expect("scenario writes");
		assert!(load_scenario_document(&path).is_err());
		let _ = std::fs::remove_file(path);
	}
}
