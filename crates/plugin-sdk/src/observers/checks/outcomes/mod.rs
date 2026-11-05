//! The checks, one file per outcome (outcomes.md). Each exports `CHECKS`, and every check
//! returns a `Verdict` that `run` turns into a `CheckResult`.

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

/// Whether the node series of `name` matching `filter` cover every window: a "no result"
/// verdict when an edge has no fresh scrape, else a note of the scrapes that failed inside the
/// windows (empty when none did), to append to the check's detail.
fn coverage(
	d: &RunData,
	name: &str,
	filter: Filter<'_>,
	windows: &[Window],
) -> Result<String, Verdict> {
	if let Some(gap) = windows.iter().find_map(|w| d.gap(name, filter, w)) {
		return Err(Verdict::new(Status::NoResult, format!("stale node metrics: {gap}")));
	}
	let failed: Vec<String> = windows
		.iter()
		.filter_map(|w| {
			let n = d.failed_scrapes(filter, w);
			(n > 0.0).then(|| format!("{} in {}", num(n), w.label))
		})
		.collect();
	Ok(if failed.is_empty() {
		String::new()
	} else {
		format!("; failed scrapes: {}", failed.join(", "))
	})
}

/// True when a window lasts at least two `interval_s`, so something recorded once per
/// interval must show up in it at least once.
fn long_enough(w: &Window, interval_s: f64) -> bool {
	w.end - w.start >= 2_000.0 * interval_s
}
