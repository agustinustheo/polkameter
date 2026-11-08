//! Run data and verdicts: window math over a run's recorded series, and the result shape a check
//! returns.

mod data;

use serde::{Deserialize, Serialize};

pub use data::{CounterReset, Label, Phase, RunData, Window, count_above, quantile};

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

/// A check's result, as summary.json has it. Plugin checks return the same shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CheckResult {
	/// Evaluate step that produced a plugin check; absent for built-in/older checks.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub source: Option<String>,
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
