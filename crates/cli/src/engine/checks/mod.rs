//! The evaluator: reads a run's recorded series and `summary.json`, runs every check, and returns
//! one verdict per check. It depends on the file formats only, so it runs on any finished run.
//!
//! A new requirement is a new entry in one of the outcome lists under `outcomes/`: add its metric
//! to the registry, record it in a monitor if none has it yet, and write the check.

mod limits;
mod outcomes;
pub mod report;
#[cfg(test)]
mod tests;

use std::ops::AddAssign;

use polkameter_plugin_sdk::observers::{
	CheckResult, CounterReset, RunData, Status, Verdict, Window, num, serde_name,
};
/// The outcomes in technical-design `test-design/outcomes.md`, plus `Run` for the ramp itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
	/// Why blocks end, build time.
	#[serde(rename = "block production")]
	BlockProduction,
	/// Relay slots, PVF time, disputes.
	Pvf,
	/// Drain, loss, refusals, pool work.
	Pool,
	/// The run itself.
	Run,
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

/// Checks without a result that a smoke run must still produce.
pub fn smoke_gaps(results: &[CheckResult]) -> Vec<String> {
	results
		.iter()
		.filter(|r| r.verdict.status == Status::NoResult && !r.optional)
		.map(|r| format!("{}: {}", r.check, r.verdict.detail))
		.collect()
}

/// Runs the checks; a counter that went down in a window gives that check no result.
///
/// A check without a result whose inputs the plan does not monitor at all (no `<monitors>`, or
/// no node of the kind it reads) is not applicable: its detail starts with `not applicable:` and
/// it is optional, so it does not make the verdict INCONCLUSIVE.
pub fn run(checks: &[Check], data: &RunData) -> Vec<CheckResult> {
	checks
		.iter()
		.map(|c| {
			let mut verdict = (c.run)(data).unwrap_or_else(|reset| {
				Verdict::new(Status::NoResult, format!("a node restarted: {reset}"))
			});
			let mut optional = c.optional;
			if verdict.status == Status::NoResult
				&& let Some(why) = outcomes::not_applicable(c.name, data)
			{
				verdict = Verdict::new(Status::NoResult, format!("not applicable: {why}"));
				optional = true;
			}
			CheckResult {
				source: None,
				outcome: serde_name(c.outcome),
				check: c.name.to_owned(),
				verdict,
				optional,
			}
		})
		.collect()
}

/// The collator's labels: each block is counted once over the collators.
pub const COLLATOR: (&str, &str) = ("job", "collator");
/// The collator's counter of why a block stopped taking txs, per `reason`.
pub const END_REASON: &str = "substrate_proposer_end_proposal_reason";

/// The collator's blocks per end reason (short names) in `w`.
pub fn end_reasons(d: &RunData, w: &Window) -> Result<Vec<(String, f64)>, CounterReset> {
	// One series per collator and reason; `diff` already sums the collators, so once per
	// reason.
	let mut reasons: Vec<&str> = d
		.series(END_REASON, &[COLLATOR])
		.iter()
		.filter_map(|s| s.labels.get("reason").map(String::as_str))
		.collect();
	reasons.sort_unstable();
	reasons.dedup();
	let mut out: Vec<(String, f64)> = Vec::new();
	for reason in reasons {
		let n = d.diff(END_REASON, &[COLLATOR, ("reason", reason)], w)?.unwrap_or(0.0);
		if n > 0.0 {
			add_to(&mut out, short_reason(reason).to_owned(), n);
		}
	}
	Ok(out)
}

/// `reason count` pairs, comma-separated: `empty 3, weight 2`.
pub fn reasons_text(r: &[(String, f64)]) -> String {
	r.iter().map(|(k, n)| format!("{k} {}", num(*n))).collect::<Vec<_>>().join(", ")
}

/// Adds `n` to the entry for `key`, or appends one.
pub fn add_to<K: PartialEq, V: AddAssign>(list: &mut Vec<(K, V)>, key: K, n: V) {
	match list.iter_mut().find(|(k, _)| *k == key) {
		Some((_, sum)) => *sum += n,
		None => list.push((key, n)),
	}
}

fn short_reason(reason: &str) -> &str {
	match reason {
		"hit_block_weight_limit" => "weight",
		"hit_block_size_limit" => "size",
		"hit_deadline" => "deadline",
		"no_more_transactions" => "empty",
		"transactions_forbidden" => "forbidden",
		other => other,
	}
}
