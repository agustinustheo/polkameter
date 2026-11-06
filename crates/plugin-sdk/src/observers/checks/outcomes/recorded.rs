//! The run itself: did every monitor record what it should.

use crate::observers::files::{registry::Outcome, to_fixed};

use crate::observers::checks::{
	Check, Status, Verdict,
	data::{CounterReset, RunData},
};

/// The stall limit when the summary has none: the default `stall-ms`.
const STALL_MS: u64 = 30_000;

fn monitors_recorded_everything(d: &RunData) -> Result<Verdict, CounterReset> {
	let s = &d.summary;
	let problems = &s.problems;
	let status = if problems.is_empty() { Status::Pass } else { Status::Warn };
	let mut detail = if problems.is_empty() {
		"no failed scrapes or unreadable blocks".to_owned()
	} else {
		problems.join("; ")
	};
	// Blocks not observed for longer than a stall takes: the stall rule could not have seen
	// one, so the run cannot say the chain kept producing blocks.
	let stall_ms = s.rules.get("stallMs").and_then(serde_json::Value::as_u64).unwrap_or(STALL_MS);
	if s.unobserved_ms > stall_ms {
		detail = format!(
			"blocks were not observed for {} s in all while reconnecting, longer than the {} s stall limit, so chain health is not established; {detail}",
			to_fixed(s.unobserved_ms as f64 / 1000.0, 1),
			to_fixed(stall_ms as f64 / 1000.0, 0),
		);
		return Ok(Verdict::new(Status::NoResult, detail));
	}
	Ok(Verdict::new(status, detail))
}

/// The run checks.
pub const CHECKS: &[Check] = &[Check {
	outcome: Outcome::Run,
	name: "monitors recorded everything",
	optional: false,
	run: monitors_recorded_everything,
}];
