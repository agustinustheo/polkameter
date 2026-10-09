//! The evaluator: reads a run's recorded series and `summary.json`, runs every check, and returns
//! one verdict per check. It depends on the file formats only, so it runs on any finished run.
//!
//! A new requirement is a new entry in one of the outcome lists under `outcomes/`: add its metric
//! to the registry, record it in a monitor if none has it yet, and write the check.

mod data;
mod limits;
mod outcomes;
pub mod report;

use polkameter_files::{registry::Outcome, serde_name};
use serde::{Deserialize, Serialize};

pub use data::{CounterReset, RunData, Window, count_above, quantile};
pub use limits::LIMITS;
pub use outcomes::all;

/// A check's status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
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

/// What a check found.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Verdict {
	/// Status.
	pub status: Status,
	/// One line for the summary table.
	pub detail: String,
	/// Numbers behind it, for summary.json.
	#[serde(default, skip_serializing_if = "Option::is_none")]
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

/// A check's result, as summary.json has it. Plugin checks return the same shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckResult {
	/// The outcome it belongs to, e.g. `pool`.
	pub outcome: String,
	/// The check.
	pub check: String,
	/// What it found.
	#[serde(flatten)]
	pub verdict: Verdict,
	/// Smoke mode does not need a result from it.
	#[serde(default, skip_serializing_if = "std::ops::Not::not")]
	pub optional: bool,
}

/// Checks without a result that a smoke run must still produce.
pub fn smoke_gaps(results: &[CheckResult]) -> Vec<String> {
	results
		.iter()
		.filter(|r| r.verdict.status == Status::NoResult && !r.optional)
		.map(|r| format!("{}: {}", r.check, r.verdict.detail))
		.collect()
}

/// Runs the checks; a counter that went down in a window gives that check no result.
pub fn run(checks: &[Check], data: &RunData) -> Vec<CheckResult> {
	checks
		.iter()
		.map(|c| CheckResult {
			outcome: serde_name(c.outcome),
			check: c.name.to_owned(),
			verdict: (c.run)(data).unwrap_or_else(|reset| {
				Verdict::new(Status::NoResult, format!("a node restarted: {reset}"))
			}),
			optional: c.optional,
		})
		.collect()
}
