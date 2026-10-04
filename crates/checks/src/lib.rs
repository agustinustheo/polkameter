//! The evaluator: reads `run.om` and `summary.json`, runs every check, and returns one verdict
//! per check. It depends on the file formats only, so it runs on any finished run.
//!
//! A new requirement is a new entry in one of the outcome lists under `outcomes/`: add its metric
//! to the registry, record it in a monitor if none has it yet, and write the check.

mod data;
mod limits;
mod outcomes;
pub mod report;

use polkameter_files::registry::Outcome;
use serde::Serialize;

pub use data::{CounterReset, RunData, Window, count_above, quantile};
pub use limits::LIMITS;
pub use outcomes::all;

/// A check's status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
	/// Within the limit.
	Pass,
	/// Close to it.
	Warn,
	/// Over it.
	Fail,
	/// A number to read, not judged.
	Info,
	/// The data is missing, or a node restarted in the window.
	#[serde(rename = "no result")]
	NoResult,
}

impl Status {
	/// The name in summary.json and summary.md.
	pub fn name(self) -> &'static str {
		match self {
			Status::Pass => "pass",
			Status::Warn => "warn",
			Status::Fail => "fail",
			Status::Info => "info",
			Status::NoResult => "no result",
		}
	}
}

/// What a check found.
#[derive(Debug, Clone, Serialize)]
pub struct Verdict {
	/// Status.
	pub status: Status,
	/// One line for the summary table.
	pub detail: String,
	/// Numbers behind it, for summary.json.
	#[serde(skip_serializing_if = "Option::is_none")]
	pub numbers: Option<serde_json::Value>,
}

impl Verdict {
	/// A verdict without numbers.
	pub fn new(status: Status, detail: impl Into<String>) -> Self {
		Self { status, detail: detail.into(), numbers: None }
	}

	/// Adds the numbers behind it.
	#[must_use]
	pub fn with(mut self, numbers: impl Serialize) -> Self {
		self.numbers = Some(serde_json::to_value(numbers).expect("numbers serialize"));
		self
	}
}

/// One check of one outcome.
#[derive(Debug, Clone, Copy)]
pub struct Check {
	/// The outcome it belongs to.
	pub outcome: Outcome,
	/// Its name in the summary.
	pub name: &'static str,
	/// The data may be absent in a normal run (no voucher loads); smoke mode doesn't need it.
	pub optional: bool,
	/// Computes the verdict.
	pub run: fn(&RunData) -> Result<Verdict, CounterReset>,
}

/// A check's result, as summary.json has it.
#[derive(Debug, Clone, Serialize)]
pub struct CheckResult {
	/// The outcome.
	pub outcome: Outcome,
	/// The check.
	pub check: &'static str,
	/// What it found.
	#[serde(flatten)]
	pub verdict: Verdict,
}

/// Checks without a result that a smoke run must still produce.
pub fn smoke_gaps(checks: &[Check], results: &[CheckResult]) -> Vec<String> {
	results
		.iter()
		.filter(|r| {
			r.verdict.status == Status::NoResult
				&& checks.iter().any(|c| c.name == r.check && !c.optional)
		})
		.map(|r| format!("{}: {}", r.check, r.verdict.detail))
		.collect()
}

/// Runs the checks; a counter that went down in a window gives that check no result.
pub fn run(checks: &[Check], data: &RunData) -> Vec<CheckResult> {
	checks
		.iter()
		.map(|c| CheckResult {
			outcome: c.outcome,
			check: c.name,
			verdict: (c.run)(data).unwrap_or_else(|reset| {
				Verdict::new(Status::NoResult, format!("a node restarted: {reset}"))
			}),
		})
		.collect()
}
