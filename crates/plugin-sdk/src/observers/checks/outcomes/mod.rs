//! The checks, one file per outcome (outcomes.md). Each exports `CHECKS`, and every check
//! returns a `Verdict` that `run` turns into a `CheckResult`.
//!
//! A check judges each window it can and collects why the others have no data. A measured
//! violation in any window decides the status (with the missing windows named in the detail);
//! only without one does missing data make the check "no result"; and a pass needs every window.

mod block_production;
mod pool;
mod pvf;
mod recorded;

use crate::observers::{
	checks::{
		Check, Status, Verdict,
		data::{Filter, RunData, Window},
	},
	files::num,
};

/// Every check, one list per outcome, in the order of the summary.
pub fn all() -> Vec<Check> {
	[block_production::CHECKS, pvf::CHECKS, pool::CHECKS, recorded::CHECKS].concat()
}

/// Per window, why the node series of `name` matching `filter` have no right value at its
/// edges (`None` when they have).
fn gaps(d: &RunData, name: &str, filter: Filter<'_>, windows: &[Window]) -> Vec<Option<String>> {
	windows
		.iter()
		.map(|w| d.gap(name, filter, w).map(|g| format!("stale node metrics: {g}")))
		.collect()
}

/// The scrapes of the nodes `filter` selects that failed inside the windows, to append to a
/// check's detail: `; failed scrapes: 2 in step 1`, or empty when none did.
fn failed_scrapes(d: &RunData, filter: Filter<'_>, windows: &[Window]) -> String {
	let failed: Vec<String> = windows
		.iter()
		.filter_map(|w| {
			let n = d.failed_scrapes(filter, w);
			(n > 0.0).then(|| format!("{} in {}", num(n), w.label))
		})
		.collect();
	if failed.is_empty() {
		String::new()
	} else {
		format!("; failed scrapes: {}", failed.join(", "))
	}
}

/// The windows without data, to append to a measured violation: `; no data for step 1 (...)`,
/// or empty when every window had data.
fn also_missing(missing: &[String]) -> String {
	if missing.is_empty() { String::new() } else { format!("; no data for {}", missing.join("; ")) }
}

/// The "no result" verdict for windows without data.
fn no_data(missing: &[String], note: &str) -> Verdict {
	Verdict::new(Status::NoResult, format!("{}{note}", missing.join("; ")))
}

/// True when a window lasts at least two `interval_s`, so something recorded once per
/// interval must show up in it at least once.
fn long_enough(w: &Window, interval_s: f64) -> bool {
	w.end - w.start >= 2_000.0 * interval_s
}

/// What a built-in check reads that the plan may not monitor at all.
enum Needs {
	/// A node of this scrape job (`collator`, `validator`, `collator-relay`).
	Job(&'static str),
	/// The relay chain recorder, which runs when the plan has `<monitors>`.
	Relay,
}

fn needs(check: &str) -> Option<Needs> {
	Some(match check {
		"why blocks end"
		| "build time within the authoring deadline"
		| "pool work leaves time for blocks" => Needs::Job("collator"),
		"relay slots (level 1)" | "no timed-out candidates or disputes" => Needs::Relay,
		"PVF time on validators (level 2, all parachains)" => Needs::Job("validator"),
		"collation funnel" => Needs::Job("collator-relay"),
		_ => return None,
	})
}

/// Why a built-in check does not apply to the run: the plan does not monitor what it reads.
/// `None` when it applies, or for a check that needs no monitor.
pub(crate) fn not_applicable(check: &str, d: &RunData) -> Option<String> {
	match needs(check)? {
		Needs::Relay if d.summary.network.para_id.is_none() => {
			Some("the plan has no <monitors>, so no relay chain is recorded".into())
		},
		Needs::Job(job) if !d.monitors_job(job) => Some(format!("no {job} node is monitored")),
		_ => None,
	}
}
