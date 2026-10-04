//! Transaction pool (outcomes.md): silent loss, stuck pool, clean refusals, pool work.

use polkameter_files::registry::Outcome;
use polkameter_files::{num, to_fixed};
use serde::Serialize;

use crate::data::{CounterReset, RunData, Window, quantile};
use crate::{Check, LIMITS, Status, Verdict};

const COLLATOR: (&str, &str) = ("job", "collator");

/// Our flood txs not yet included, refused or expired, from the load tool's own counters.
fn backlog(d: &RunData, t: f64) -> f64 {
	let v = |name| d.at(name, &[], t).unwrap_or(0.0);
	v("polkameter_tx_sent_total")
		- v("polkameter_tx_included_total")
		- v("polkameter_tx_rejected_total")
		- v("polkameter_tx_expired_total")
}

/// Blocks that ended empty (`no_more_transactions`) while more than `min` of our txs waited and
/// none of ours went in: the pool gave the builder nothing it had. A block of heavy txs also ends
/// with `no_more_transactions` when the builder skipped every waiting tx that didn't fit; then
/// some of ours went in, so it doesn't count.
pub fn empty_while_waiting(d: &RunData, w: &Window, min: f64) -> f64 {
	let included = |t| d.at("polkameter_tx_included_total", &[], t).unwrap_or(0.0);
	let mut n = 0.0;
	for s in d.series(
		"substrate_proposer_end_proposal_reason",
		&[COLLATOR, ("reason", "no_more_transactions")],
	) {
		let pts: Vec<_> = s.points.iter().filter(|p| p.t >= w.start && p.t <= w.end).collect();
		for pair in pts.windows(2) {
			let (a, b) = (pair[0], pair[1]);
			let waited = backlog(d, a.t).min(backlog(d, b.t));
			if b.value > a.value && waited > min && included(b.t) == included(a.t) {
				n += b.value - a.value;
			}
		}
	}
	n
}

fn no_silent_loss(d: &RunData) -> Result<Verdict, CounterReset> {
	let l = &d.summary.loss;
	let Some(lost) = l.lost else {
		return Ok(Verdict::new(
			Status::NoResult,
			l.note.clone().unwrap_or_else(|| "the pool could not be listed".into()),
		));
	};
	let pool = l.node_pool.as_ref().map_or(String::new(), |p| {
		format!("; the node's mempool holds {} txs, {} ready", p.mempool, p.ready)
	});
	let status = if lost > 0 { Status::Fail } else { Status::Pass };
	Ok(Verdict::new(
		status,
		format!("{lost} of {} txs accepted, in no block, and without any error{pool}", l.sent),
	))
}

fn burst_drains(d: &RunData) -> Result<Verdict, CounterReset> {
	let r = &d.summary.recovery;
	let Some(rec) = d.phase("recovery").filter(|_| r.measured) else {
		return Ok(Verdict::new(Status::NoResult, r.detail.clone()));
	};
	let empty = empty_while_waiting(d, &rec, LIMITS.pool_stuck_min_txs);
	let back = match r.seconds {
		Some(s) if r.recovered => format!("probes back in time {s} s after the load stopped"),
		_ => format!("not back after the recovery budget ({})", r.detail),
	};
	let drained = match r.drained_seconds {
		Some(s) => format!("drained after {s} s"),
		None => format!("{} left at the end", r.backlog_at_end),
	};
	let stuck = if empty > 0.0 {
		format!(
			"; {empty} blocks ended empty while more than {} of our txs waited",
			LIMITS.pool_stuck_min_txs
		)
	} else {
		String::new()
	};
	let ok = r.recovered && r.drained_seconds.is_some() && empty == 0.0;
	let detail = format!("{back}; backlog {} at the stop, {drained}{stuck}", r.backlog_at_stop);
	Ok(Verdict::new(if ok { Status::Pass } else { Status::Fail }, detail)
		.with(serde_json::json!({ "drainedSeconds": r.drained_seconds })))
}

fn refusals_clean(d: &RunData) -> Result<Verdict, CounterReset> {
	let w = d.run();
	let mut by_reason = Vec::new();
	for s in d.series("polkameter_tx_rejected_total", &[]) {
		let filter: Vec<(&str, &str)> =
			s.labels.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
		let n = d.diff("polkameter_tx_rejected_total", &filter, &w)?.unwrap_or(0.0);
		by_reason.push((s.labels.get("reason").cloned().unwrap_or_default(), n));
	}
	let no_code = by_reason.iter().filter(|(r, _)| r == "no code").fold(0.0, |a, x| a + x.1);
	let total = by_reason.iter().fold(0.0, |a, x| a + x.1);
	let hung = d.summary.stop.rule == polkameter_files::summary::Rule::PoolIntake;
	let reasons = if by_reason.is_empty() {
		"none".to_owned()
	} else {
		by_reason
			.iter()
			.map(|(r, n)| format!("{r}: {n}"))
			.collect::<Vec<_>>()
			.join(", ")
	};
	let hung_detail = if hung { format!("; {}", d.summary.stop.detail) } else { String::new() };
	let status = if no_code > 0.0 {
		Status::Fail
	} else if hung {
		Status::Warn
	} else {
		Status::Pass
	};
	Ok(Verdict::new(status, format!("{total} refused ({reasons}){hung_detail}")))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PoolWork {
	window: String,
	maintain_p95_s: Option<f64>,
	backlog: f64,
}

fn pool_work(d: &RunData) -> Result<Verdict, CounterReset> {
	if !d.has("substrate_sub_txpool_maintain_duration_seconds_bucket", &[COLLATOR]) {
		return Ok(Verdict::new(Status::NoResult, "no collator metrics"));
	}
	let budget = LIMITS.max_maintain_share_of_block * d.block_interval_s;
	let mut windows = Vec::new();
	for w in d.load_windows() {
		let scheduled = d
			.at("substrate_sub_txpool_validations_scheduled", &[COLLATOR], w.end)
			.unwrap_or(0.0);
		let finished = d
			.at("substrate_sub_txpool_validations_finished", &[COLLATOR], w.end)
			.unwrap_or(0.0);
		let b = d.buckets("substrate_sub_txpool_maintain_duration_seconds", &[COLLATOR], &w)?;
		windows.push(PoolWork {
			maintain_p95_s: quantile(b.as_ref(), 0.95),
			backlog: scheduled - finished,
			window: w.label,
		});
	}
	let slow = windows.iter().find(|s| s.maintain_p95_s.unwrap_or(0.0) > budget);
	let backed = windows.iter().find(|s| s.backlog > LIMITS.max_validation_backlog);
	let share = LIMITS.max_maintain_share_of_block * 100.0;
	let (status, detail) = match (slow, backed) {
		(Some(s), _) => (
			Status::Fail,
			format!(
				"{}: maintenance p95 in the {} s bucket, over {} s ({share}% of a block)",
				s.window,
				num(s.maintain_p95_s.unwrap_or(0.0)),
				to_fixed(budget, 1)
			),
		),
		(None, Some(b)) => {
			(Status::Warn, format!("{}: {} txs waiting for validation", b.window, b.backlog))
		},
		(None, None) => (
			Status::Pass,
			format!(
				"maintenance p95 within {} s in every step and in recovery",
				to_fixed(budget, 1)
			),
		),
	};
	Ok(Verdict::new(status, detail).with(serde_json::json!({ "windows": windows })))
}

/// The pool checks.
pub const CHECKS: &[Check] = &[
	Check { outcome: Outcome::Pool, name: "no silent loss", optional: false, run: no_silent_loss },
	Check { outcome: Outcome::Pool, name: "the burst drains", optional: false, run: burst_drains },
	Check {
		outcome: Outcome::Pool,
		name: "refusals are clean",
		optional: false,
		run: refusals_clean,
	},
	Check {
		outcome: Outcome::Pool,
		name: "pool work leaves time for blocks",
		optional: false,
		run: pool_work,
	},
];
