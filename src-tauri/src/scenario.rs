use serde::{Deserialize, Serialize};

pub const SCENARIO_VERSION: u32 = 1;

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ScenarioDocument {
	pub version: u32,
	pub test_plan: TestPlan,
	pub chain: ChainTarget,
	pub signer_source: DevSignerSource,
	pub thread_groups: Vec<ThreadGroup>,
	pub collectors: Vec<Collector>,
}

impl ScenarioDocument {
	pub fn migrate(mut self) -> Result<Self, String> {
		match self.version {
			SCENARIO_VERSION => Ok(self),
			0 => {
				self.version = SCENARIO_VERSION;
				Ok(self)
			},
			version => {
				Err(format!("unsupported scenario version {version}; expected {SCENARIO_VERSION}"))
			},
		}
	}

	pub fn validate(&self) -> Vec<ValidationIssue> {
		let mut issues = Vec::new();
		if self.version != SCENARIO_VERSION {
			issues.push(ValidationIssue::new(
				"version",
				format!(
					"unsupported scenario version {}; expected {SCENARIO_VERSION}",
					self.version
				),
			));
		}
		if self.test_plan.name.trim().is_empty() {
			issues.push(ValidationIssue::new("testPlan.name", "must not be empty"));
		}
		if self.test_plan.limits.whole_run_timeout_ms < 1_000 {
			issues.push(ValidationIssue::new(
				"testPlan.limits.wholeRunTimeoutMs",
				"must be at least 1000 ms",
			));
		}
		if self.test_plan.limits.shutdown_drain_timeout_ms < 1_000 {
			issues.push(ValidationIssue::new(
				"testPlan.limits.shutdownDrainTimeoutMs",
				"must be at least 1000 ms",
			));
		}
		if !(self.chain.endpoint.starts_with("ws://") || self.chain.endpoint.starts_with("wss://"))
		{
			issues.push(ValidationIssue::new("chain.endpoint", "must use ws:// or wss://"));
		}
		if self.signer_source.base_suri.trim().is_empty() {
			issues.push(ValidationIssue::new("signerSource.baseSuri", "must not be empty"));
		}
		if self.thread_groups.is_empty() {
			issues.push(ValidationIssue::new("threadGroups", "must contain at least one group"));
		}

		for (index, group) in self.thread_groups.iter().enumerate() {
			let prefix = format!("threadGroups[{index}]");
			if group.name.trim().is_empty() {
				issues.push(ValidationIssue::new(format!("{prefix}.name"), "must not be empty"));
			}
			if group.users == 0 {
				issues
					.push(ValidationIssue::new(format!("{prefix}.users"), "must be at least one"));
			}
			if group.concurrency == 0 || group.concurrency > group.users {
				issues.push(ValidationIssue::new(
					format!("{prefix}.concurrency"),
					"must be between one and the virtual-user count",
				));
			}
			if let Err(message) = group.arrival.validate() {
				issues.push(ValidationIssue::new(format!("{prefix}.arrival"), message));
			}
			if group.samplers.is_empty() {
				issues
					.push(ValidationIssue::new(format!("{prefix}.samplers"), "must not be empty"));
			}
			for (sampler_index, sampler) in group.samplers.iter().enumerate() {
				let sampler_prefix = format!("{prefix}.samplers[{sampler_index}]");
				if sampler.label.trim().is_empty() {
					issues.push(ValidationIssue::new(
						format!("{sampler_prefix}.label"),
						"must not be empty",
					));
				}
				if sampler.pallet.trim().is_empty() || sampler.call.trim().is_empty() {
					issues.push(ValidationIssue::new(
						format!("{sampler_prefix}.call"),
						"pallet and call must not be empty",
					));
				}
				if !sampler.arguments.is_object() && !sampler.arguments.is_array() {
					issues.push(ValidationIssue::new(
						format!("{sampler_prefix}.arguments"),
						"must be a JSON object or array",
					));
				}
				if !sampler.mortality_period.is_power_of_two() || sampler.mortality_period < 4 {
					issues.push(ValidationIssue::new(
						format!("{sampler_prefix}.mortalityPeriod"),
						"must be a power of two of at least four",
					));
				}
				if sampler.finality_timeout_ms < 1_000 {
					issues.push(ValidationIssue::new(
						format!("{sampler_prefix}.finalityTimeoutMs"),
						"must be at least 1000 ms",
					));
				}
			}
		}

		issues
	}

	pub fn redacted_clone(&self) -> Self {
		let mut document = self.clone();
		document.signer_source.base_suri = "[redacted]".into();
		document
	}
}

pub fn signer_derivation_root(document: &ScenarioDocument, run_id: &str) -> String {
	format!("{}//run-{run_id}", document.signer_source.derivation_path)
}

pub fn signer_suri(document: &ScenarioDocument, index: u32, run_id: &str) -> String {
	if document.signer_source.derivation_path.is_empty() && index == 0 {
		return document.signer_source.base_suri.clone();
	}
	format!(
		"{}{}//{index}",
		document.signer_source.base_suri,
		signer_derivation_root(document, run_id)
	)
}

pub fn required_signer_count(document: &ScenarioDocument) -> u32 {
	document.thread_groups.iter().map(|group| group.users).sum()
}

pub fn signer_offset(document: &ScenarioDocument, group_position: usize) -> u32 {
	document
		.thread_groups
		.iter()
		.take(group_position)
		.map(|group| group.users)
		.sum()
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestPlan {
	pub name: String,
	pub description: String,
	pub seed: u64,
	#[serde(default)]
	pub limits: RunLimits,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunLimits {
	pub whole_run_timeout_ms: u64,
	pub shutdown_drain_timeout_ms: u64,
}

impl Default for RunLimits {
	fn default() -> Self {
		Self { whole_run_timeout_ms: 900_000, shutdown_drain_timeout_ms: 300_000 }
	}
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChainTarget {
	pub endpoint: String,
	#[serde(default = "default_transaction_profile")]
	pub transaction_profile: TransactionProfile,
}

fn default_transaction_profile() -> TransactionProfile {
	TransactionProfile::Polkadot
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TransactionProfile {
	Polkadot,
	Custom(String),
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DevSignerSource {
	pub base_suri: String,
	#[serde(default = "default_derivation_path")]
	pub derivation_path: String,
}

fn default_derivation_path() -> String {
	"//polkameter".into()
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadGroup {
	pub name: String,
	pub users: u32,
	pub concurrency: u32,
	pub arrival: ArrivalModel,
	pub samplers: Vec<TransactionSampler>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum ArrivalModel {
	Burst { window_ms: u64 },
	Ramp { duration_ms: u64 },
	Poisson { rate_per_second: f64 },
}

impl ArrivalModel {
	pub fn validate(&self) -> Result<(), &'static str> {
		match self {
			Self::Burst { window_ms } if *window_ms == 0 => {
				Err("burst window must be at least 1 ms")
			},
			Self::Ramp { duration_ms } if *duration_ms == 0 => {
				Err("ramp duration must be at least 1 ms")
			},
			Self::Poisson { rate_per_second } if *rate_per_second <= 0.0 => {
				Err("Poisson rate must be positive")
			},
			_ => Ok(()),
		}
	}
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SamplerPhase {
	Setup,
	Transaction,
	Teardown,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransactionSampler {
	pub phase: SamplerPhase,
	pub label: String,
	pub pallet: String,
	pub call: String,
	pub arguments: serde_json::Value,
	pub completion: CompletionBoundary,
	pub mortality_period: u32,
	pub finality_timeout_ms: u64,
	#[serde(default)]
	pub assertions: Vec<Assertion>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletionBoundary {
	Submitted,
	InBlock,
	Finalized,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum Assertion {
	Success,
	MaxElapsed { milliseconds: u64 },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Collector {
	Jtl,
	EventsJsonl,
	TelemetryJsonl,
	Summary,
	SvgPlots,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ValidationIssue {
	pub field: String,
	pub message: String,
}

impl ValidationIssue {
	pub fn new(field: impl Into<String>, message: impl Into<String>) -> Self {
		Self { field: field.into(), message: message.into() }
	}
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvedPlan {
	pub scenario: ScenarioDocument,
	pub run_id: String,
	pub signer_derivation_root: String,
	pub required_signer_count: u32,
	pub scheduled_samples: u64,
	pub scheduler: String,
}

impl ResolvedPlan {
	pub fn from_scenario(scenario: &ScenarioDocument, run_id: impl Into<String>) -> Self {
		let scheduled_samples = scenario
			.thread_groups
			.iter()
			.map(|group| u64::from(group.users) * group.samplers.len() as u64)
			.sum();
		let run_id = run_id.into();
		Self {
			scenario: scenario.redacted_clone(),
			signer_derivation_root: signer_derivation_root(scenario, &run_id),
			required_signer_count: scenario.thread_groups.iter().map(|group| group.users).sum(),
			run_id,
			scheduled_samples,
			scheduler: "seeded deterministic arrival offsets with bounded concurrency".into(),
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn scenario() -> ScenarioDocument {
		ScenarioDocument {
			version: SCENARIO_VERSION,
			test_plan: TestPlan {
				name: "Transfer".into(),
				description: String::new(),
				seed: 7,
				limits: RunLimits::default(),
			},
			chain: ChainTarget {
				endpoint: "ws://127.0.0.1:9944".into(),
				transaction_profile: TransactionProfile::Polkadot,
			},
			signer_source: DevSignerSource {
				base_suri: "//Alice".into(),
				derivation_path: "//polkameter".into(),
			},
			thread_groups: vec![ThreadGroup {
				name: "Users".into(),
				users: 10,
				concurrency: 5,
				arrival: ArrivalModel::Burst { window_ms: 1_000 },
				samplers: vec![TransactionSampler {
					phase: SamplerPhase::Transaction,
					label: "transfer".into(),
					pallet: "Balances".into(),
					call: "transfer_keep_alive".into(),
					arguments: serde_json::json!({"dest": "5GrwvaEF...", "value": "1000000000"}),
					completion: CompletionBoundary::Finalized,
					mortality_period: 4_096,
					finality_timeout_ms: 300_000,
					assertions: vec![Assertion::Success],
				}],
			}],
			collectors: vec![Collector::Jtl],
		}
	}

	#[test]
	fn resolved_plan_never_contains_the_dev_suri() {
		let plan = ResolvedPlan::from_scenario(&scenario(), "run-1");
		let encoded = serde_json::to_string(&plan).expect("serializable plan");
		assert!(!encoded.contains("//Alice"));
		assert!(encoded.contains("[redacted]"));
		assert_eq!(plan.signer_derivation_root, "//polkameter//run-run-1");
		assert_eq!(plan.required_signer_count, 10);
	}

	#[test]
	fn signers_are_stable_within_a_run_and_distinct_across_runs() {
		let document = scenario();
		assert_eq!(signer_suri(&document, 3, "run-a"), signer_suri(&document, 3, "run-a"));
		assert_ne!(signer_suri(&document, 3, "run-a"), signer_suri(&document, 3, "run-b"));
	}

	#[test]
	fn scenario_version_is_checked() {
		let mut document = scenario();
		document.version = 99;
		assert!(document.validate().iter().any(|issue| issue.field == "version"));
	}

	#[test]
	fn version_zero_migrates_without_changing_the_plan() {
		let mut document = scenario();
		document.version = 0;
		let migrated = document.migrate().expect("version zero is migratable");
		assert_eq!(migrated.version, SCENARIO_VERSION);
		assert_eq!(migrated.test_plan.name, "Transfer");
	}

	#[test]
	fn whole_run_and_drain_limits_are_validated() {
		let mut document = scenario();
		document.test_plan.limits.whole_run_timeout_ms = 1;
		document.test_plan.limits.shutdown_drain_timeout_ms = 1;
		let fields = document.validate().into_iter().map(|issue| issue.field).collect::<Vec<_>>();
		assert!(fields.contains(&"testPlan.limits.wholeRunTimeoutMs".into()));
		assert!(fields.contains(&"testPlan.limits.shutdownDrainTimeoutMs".into()));
	}
}
