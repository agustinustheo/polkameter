//! Block production (outcomes.md): why blocks end, and build time against the authoring deadline.

use crate::observers::files::registry::Outcome;
use serde::Serialize;

use crate::observers::checks::{
	Check, LIMITS, Status, Verdict,
	data::{
		COLLATOR, CounterReset, END_REASON, Label, RunData, count_above, quantile, reasons_text,
	},
	outcomes::{also_missing, failed_scrapes, gaps, long_enough, no_data},
};

const BUILD_TIME: &str = "substrate_proposer_block_constructed";

fn text(r: &[(String, f64)]) -> String {
	if r.is_empty() { "no blocks".into() } else { reasons_text(r) }
}

#[derive(Serialize)]
struct Reasons {
	window: Label,
	#[serde(serialize_with = "as_map")]
	reasons: Vec<(String, f64)>,
}

fn as_map<S: serde::Serializer>(r: &[(String, f64)], s: S) -> Result<S::Ok, S::Error> {
	s.collect_map(r.iter().map(|(k, v)| (k, v)))
}

fn why_blocks_end(d: &RunData) -> Result<Verdict, CounterReset> {
	if !d.has(END_REASON, &[]) {
		return Ok(Verdict::new(Status::NoResult, "no collator metrics"));
	}
	let load = d.load_windows();
	let note = failed_scrapes(d, &[COLLATOR], &load);
	let mut missing = Vec::new();
	let mut windows = Vec::new();
	for (w, gap) in load.iter().zip(gaps(d, END_REASON, &[COLLATOR], &load)) {
		if let Some(gap) = gap {
			missing.push(gap);
			continue;
		}
		match d.end_reasons(w) {
			Ok(reasons) => windows.push(Reasons { reasons, window: w.label }),
			Err(reset) => missing.push(format!("{}: a node restarted: {reset}", w.label)),
		}
	}
	if windows.iter().all(|w| w.reasons.is_empty()) {
		if missing.is_empty() {
			missing.push("the collator recorded no block in any step or in recovery".into());
		}
		return Ok(no_data(&missing, &note));
	}
	let full: Vec<_> = windows
		.iter()
		.filter(|s| {
			["weight", "size", "deadline"]
				.iter()
				.any(|k| s.reasons.iter().any(|(r, _)| r == k))
		})
		.collect();
	let found = if full.is_empty() {
		"no block ended full".to_owned()
	} else {
		format!(
			"blocks end full in {}",
			full.iter()
				.map(|s| format!("{} ({})", s.window, text(&s.reasons)))
				.collect::<Vec<_>>()
				.join("; ")
		)
	};
	// Not judged, so a window without data only adds to the detail.
	let detail = format!("{found}{}{note}", also_missing(&missing));
	Ok(Verdict::new(Status::Info, detail).with(serde_json::json!({ "windows": windows })))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BuildTime {
	window: Label,
	blocks: f64,
	p95_s: Option<f64>,
	over: f64,
}

/// The node's buckets jump from 1 s to 2.5 s, so only a build over 2.5 s is surely over the 2 s
/// deadline: over 5% of blocks is a fail, any such block or a p95 of 2.5 s a warning.
///
/// A window has no data without a right value at its edges, after a node restart, or when it
/// lasts two block intervals or more without a built block. A failure or warning in the windows
/// with data stands; otherwise any window without data makes the check "no result".
fn build_time(d: &RunData) -> Result<Verdict, CounterReset> {
	let bucket = format!("{BUILD_TIME}_bucket");
	if !d.has(&bucket, &[]) {
		return Ok(Verdict::new(Status::NoResult, "no collator metrics"));
	}
	let load = d.load_windows();
	let note = failed_scrapes(d, &[COLLATOR], &load);
	let mut windows = Vec::new();
	let mut missing = Vec::new();
	for (w, gap) in load.iter().zip(gaps(d, &bucket, &[COLLATOR], &load)) {
		if let Some(gap) = gap {
			missing.push(gap);
			continue;
		}
		let b = match d.buckets(BUILD_TIME, &[COLLATOR], w) {
			Ok(b) => b,
			Err(reset) => {
				missing.push(format!("{}: a node restarted: {reset}", w.label));
				continue;
			},
		};
		let blocks = b.as_ref().and_then(|b| b.last()).map_or(0.0, |x| x.1);
		if blocks == 0.0 && long_enough(w, d.block_interval_s) {
			missing.push(format!("{}: the collator recorded no built block", w.label));
			continue;
		}
		windows.push(BuildTime {
			window: w.label,
			blocks,
			p95_s: quantile(b.as_ref(), 0.95),
			over: count_above(b.as_ref(), 2.5).unwrap_or(0.0),
		});
	}
	let numbers = serde_json::json!({ "windows": windows });
	let deadline = LIMITS.authoring_deadline_s;
	let failed = windows.iter().find(|s| s.blocks > 0.0 && s.over / s.blocks > 0.05);
	let close: Vec<_> = windows
		.iter()
		.filter(|s| s.over > 0.0 || s.p95_s == Some(2.5))
		.map(|s| s.window.to_string())
		.collect();
	let also = also_missing(&missing);
	let (status, detail) = match failed {
		Some(f) => (
			Status::Fail,
			format!(
				"{}: {} of {} blocks took over 2.5 s (deadline {deadline} s){also}",
				f.window, f.over, f.blocks
			),
		),
		None if !close.is_empty() => (
			Status::Warn,
			format!(
				"{}: p95 between 1 and 2.5 s or a block over 2.5 s (deadline {deadline} s){also}",
				close.join(", ")
			),
		),
		None if !missing.is_empty() => return Ok(no_data(&missing, &note).with(numbers)),
		None if windows.iter().all(|w| w.blocks == 0.0) => {
			let none = ["the collator recorded no built block in any window".to_owned()];
			return Ok(no_data(&none, &note).with(numbers));
		},
		None => {
			(Status::Pass, "p95 build time at most 1 s in every step and in recovery".to_owned())
		},
	};
	Ok(Verdict::new(status, detail + &note).with(numbers))
}

/// The block production checks.
pub const CHECKS: &[Check] = &[
	Check {
		outcome: Outcome::BlockProduction,
		name: "why blocks end",
		optional: false,
		run: why_blocks_end,
	},
	Check {
		outcome: Outcome::BlockProduction,
		name: "build time within the authoring deadline",
		optional: false,
		run: build_time,
	},
];
