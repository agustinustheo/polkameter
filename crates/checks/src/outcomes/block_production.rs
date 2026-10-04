//! Block production (outcomes.md): why blocks end, and build time against the authoring deadline.

use polkameter_files::registry::Outcome;
use serde::Serialize;

use crate::data::{CounterReset, RunData, count_above, quantile};
use crate::{Check, LIMITS, Status, Verdict};

const COLLATOR: (&str, &str) = ("job", "collator");
const END_REASON: &str = "substrate_proposer_end_proposal_reason";
const BUILD_TIME: &str = "substrate_proposer_block_constructed";

fn text(r: &[(String, f64)]) -> String {
	if r.is_empty() {
		return "no blocks".into();
	}
	r.iter().map(|(k, n)| format!("{k} {n}")).collect::<Vec<_>>().join(", ")
}

#[derive(Serialize)]
struct Reasons {
	window: String,
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
	let windows = d
		.load_windows()
		.into_iter()
		.map(|w| Ok(Reasons { reasons: d.end_reasons(&w)?, window: w.label }))
		.collect::<Result<Vec<_>, _>>()?;
	let full: Vec<_> = windows
		.iter()
		.filter(|s| {
			["weight", "size", "deadline"]
				.iter()
				.any(|k| s.reasons.iter().any(|(r, _)| r == k))
		})
		.collect();
	let detail = if full.is_empty() {
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
	Ok(Verdict::new(Status::Info, detail).with(serde_json::json!({ "windows": windows })))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct BuildTime {
	window: String,
	blocks: f64,
	p95_s: Option<f64>,
	over: f64,
}

/// The node's buckets jump from 1 s to 2.5 s, so only a build over 2.5 s is surely over the 2 s
/// deadline: over 5% of blocks is a fail, any such block or a p95 of 2.5 s a warning.
fn build_time(d: &RunData) -> Result<Verdict, CounterReset> {
	if !d.has(&format!("{BUILD_TIME}_bucket"), &[]) {
		return Ok(Verdict::new(Status::NoResult, "no collator metrics"));
	}
	let mut windows = Vec::new();
	for w in d.load_windows() {
		let b = d.buckets(BUILD_TIME, &[COLLATOR], &w)?;
		let blocks = b.as_ref().and_then(|b| b.last()).map_or(0.0, |x| x.1);
		windows.push(BuildTime {
			window: w.label,
			blocks,
			p95_s: quantile(b.as_ref(), 0.95),
			over: count_above(b.as_ref(), 2.5).unwrap_or(0.0),
		});
	}
	let deadline = LIMITS.authoring_deadline_s;
	let failed = windows.iter().find(|s| s.blocks > 0.0 && s.over / s.blocks > 0.05);
	let close: Vec<_> = windows
		.iter()
		.filter(|s| s.over > 0.0 || s.p95_s == Some(2.5))
		.map(|s| s.window.as_str())
		.collect();
	let (status, detail) = match failed {
		Some(f) => (
			Status::Fail,
			format!(
				"{}: {} of {} blocks took over 2.5 s (deadline {deadline} s)",
				f.window, f.over, f.blocks
			),
		),
		None if !close.is_empty() => (
			Status::Warn,
			format!(
				"{}: p95 between 1 and 2.5 s or a block over 2.5 s (deadline {deadline} s)",
				close.join(", ")
			),
		),
		None => {
			(Status::Pass, "p95 build time at most 1 s in every step and in recovery".to_owned())
		},
	};
	Ok(Verdict::new(status, detail).with(serde_json::json!({ "windows": windows })))
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
