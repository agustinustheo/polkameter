//! The run itself: did every monitor record what it should.

use polkameter_files::registry::Outcome;

use crate::data::{CounterReset, RunData};
use crate::{Check, Status, Verdict};

fn monitors_recorded_everything(d: &RunData) -> Result<Verdict, CounterReset> {
	let problems = &d.summary.problems;
	let status = if problems.is_empty() { Status::Pass } else { Status::Warn };
	let detail = if problems.is_empty() {
		"no failed scrapes or unreadable blocks".to_owned()
	} else {
		problems.join("; ")
	};
	Ok(Verdict::new(status, detail))
}

/// The run checks.
pub const CHECKS: &[Check] = &[Check {
	outcome: Outcome::Run,
	name: "monitors recorded everything",
	optional: false,
	run: monitors_recorded_everything,
}];
