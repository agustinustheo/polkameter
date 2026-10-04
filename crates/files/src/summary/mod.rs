//! `summary.json`: one typed schema, written by the report and read by the evaluator.
//! `schemaVersion` 1 is this layout.

use serde::{Deserialize, Serialize};
use serde_json::Value;

mod after;

pub use after::{
	Baseline, Finality, Loss, LostTx, NodePool, OnChain, Outcome, Probe, ProbePhase, Recovery,
	StateSample,
};

/// The layout written here.
pub const SCHEMA_VERSION: u32 = 1;

/// Failure classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Class {
	/// The node refuses or delays our txs and keeps making blocks.
	Graceful,
	/// Block production or finality degrades for everyone.
	Hard,
	/// A tx is gone with no error, or landed and left no state.
	Silent,
}

/// Why the load ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[allow(missing_docs)]
pub enum Rule {
	#[serde(rename = "pool refuses")]
	PoolRefuses,
	#[serde(rename = "pool intake")]
	PoolIntake,
	#[serde(rename = "slow blocks")]
	SlowBlocks,
	#[serde(rename = "stall")]
	Stall,
	#[serde(rename = "finality stall")]
	FinalityStall,
	#[serde(rename = "node down")]
	NodeDown,
	#[serde(rename = "rate cap")]
	RateCap,
	#[serde(rename = "budget used up")]
	BudgetUsedUp,
	#[serde(rename = "generator limit")]
	GeneratorLimit,
	#[serde(rename = "smoke error")]
	SmokeError,
}

impl Class {
	/// The name in summary.json and summary.md.
	pub fn name(self) -> &'static str {
		match self {
			Class::Graceful => "graceful",
			Class::Hard => "hard",
			Class::Silent => "silent",
		}
	}
}

impl Rule {
	/// The name in summary.json and summary.md.
	pub fn name(self) -> &'static str {
		match self {
			Rule::PoolRefuses => "pool refuses",
			Rule::PoolIntake => "pool intake",
			Rule::SlowBlocks => "slow blocks",
			Rule::Stall => "stall",
			Rule::FinalityStall => "finality stall",
			Rule::NodeDown => "node down",
			Rule::RateCap => "rate cap",
			Rule::BudgetUsedUp => "budget used up",
			Rule::GeneratorLimit => "generator limit",
			Rule::SmokeError => "smoke error",
		}
	}

	/// The failure class; `None` for an end that is not a failure.
	pub fn class(self) -> Option<Class> {
		match self {
			Rule::PoolRefuses | Rule::PoolIntake => Some(Class::Graceful),
			Rule::SlowBlocks | Rule::Stall | Rule::FinalityStall | Rule::NodeDown => {
				Some(Class::Hard)
			},
			Rule::RateCap | Rule::BudgetUsedUp | Rule::GeneratorLimit | Rule::SmokeError => None,
		}
	}
}

/// Why and where the load ended.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Stop {
	/// The rule.
	pub rule: Rule,
	/// What it saw.
	pub detail: String,
	/// The step.
	pub step: Option<u32>,
	/// Its class.
	pub class: Option<Class>,
}

impl Stop {
	/// A stop by `rule` in `step`.
	pub fn new(rule: Rule, step: Option<u32>, detail: impl Into<String>) -> Self {
		Stop { rule, detail: detail.into(), step, class: rule.class() }
	}
}

/// The first response measure a step violated.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BreakingPoint {
	/// Step.
	pub step: u32,
	/// Its target rate.
	pub target_rate: f64,
	/// included, latency or refused.
	pub measure: String,
	/// What it saw.
	pub detail: String,
}

/// The step before the breaking point.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MaxSustained {
	/// Step.
	pub step: u32,
	/// Its target rate.
	pub target_rate: f64,
	/// What the chain took.
	pub included_per_s: f64,
}

/// One failure of a run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FailureMode {
	/// Its class.
	pub class: Class,
	/// What happened.
	pub what: String,
}

/// The chain under load, as the run found it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Network {
	/// RPC URL of the node the load is submitted to.
	pub url: String,
	/// The parachain the relay checks judge; absent when the run observes no relay.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub para_id: Option<u32>,
	/// Runtime spec.
	pub spec_version: u32,
	/// Block interval before the run.
	pub block_interval_s: f64,
}

/// The machine the load tool ran on.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Runner {
	/// CPUs.
	pub cpus: usize,
	/// CPU model.
	pub cpu_model: Option<String>,
	/// Memory.
	pub mem_gi_b: u64,
}

/// The isolated component exercised by a polkameter-test scenario.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Artifact {
	/// Runtime call or other component name.
	pub name: String,
	/// What one submitted operation does.
	pub description: String,
	/// Why the operation matters to the product flow.
	pub context: String,
}

/// `summary.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Summary {
	/// [`SCHEMA_VERSION`].
	pub schema_version: u32,
	/// Scenario title.
	pub scenario: String,
	/// The isolated component under stress.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub artifact: Option<Artifact>,
	/// Run id.
	pub run_id: String,
	/// smoke or stress.
	pub mode: String,
	/// Options as given.
	pub params: Value,
	/// What the run could spend.
	pub budget: String,
	/// Setup time.
	pub setup_seconds: u64,
	/// Scenario numbers.
	pub extra: Value,
	/// The thresholds.
	pub rules: Value,
	/// Why the load ended.
	pub stop: Stop,
	/// The first violated measure.
	pub breaking_point: Option<BreakingPoint>,
	/// The step before it.
	pub max_sustained: Option<MaxSustained>,
	/// The failures.
	pub failure_modes: Vec<FailureMode>,
	/// After the load.
	pub recovery: Recovery,
	/// The loss check.
	pub loss: Loss,
	/// Before the load.
	pub baseline: Baseline,
	/// What the monitors could not record.
	pub problems: Vec<String>,
	/// The chain under load.
	pub network: Network,
	/// The load tool's machine.
	pub runner: Runner,
	/// Steps run.
	pub steps: usize,
	/// Check results (the report adds them).
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub checks: Option<Value>,
}
