use std::path::Path;

use crate::scenario::ScenarioDocument;

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
		let suri = std::env::var(variable)
			.map_err(|_| format!("signer environment variable {variable} is not set"))?;
		validate_resolved_suri(document, &suri, variable)?;
		document.signer_source.base_suri = suri;
		return Ok(());
	}
	crate::resolve_signer_profile(document)
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

	#[test]
	fn resolved_signers_must_be_nonempty_and_support_development_funding() {
		let mut document = crate::artifacts::test_scenario();
		assert!(validate_resolved_suri(&document, "", "POLKAMETER_SURI").is_err());
		document.signer_source.funding = Some(crate::scenario::DevelopmentFunding {
			amount: "1000000000".into(),
			finality_timeout_ms: 60_000,
			batch_size: 50,
		});
		assert!(validate_resolved_suri(&document, "seed phrase", "POLKAMETER_SURI").is_err());
		assert!(validate_resolved_suri(&document, "//Alice", "POLKAMETER_SURI").is_ok());
	}
}
