//! Recycler maintenance (outcomes.md, "Faults we check"): backlog, maintenance, cleanup, and
//! the time from a voucher load to a built root.

use polkameter_files::num;
use polkameter_files::registry::Outcome;

use crate::data::{CounterReset, RunData, Window, quantile};
use crate::{Check, Status, Verdict};

const NO_DATA: &str = "no Recycler data (no Members pallet?)";

fn sum(d: &RunData, name: &str, t: f64, filter: &[(&str, &str)]) -> f64 {
	d.at(name, filter, t).unwrap_or(0.0)
}

/// Queued, unbuilt, stale rings at `t`.
fn backlog(d: &RunData, t: f64) -> (f64, f64, f64) {
	(
		sum(d, "polkameter_recycler_queued_keys", t, &[]),
		sum(d, "polkameter_recycler_unbuilt_keys", t, &[]),
		sum(d, "polkameter_recycler_stale_rings", t, &[]),
	)
}

/// From the chain recorder's first Recycler read to its last. Our gauges have no value before
/// the first, and the run waits for the Recycler after the loss check, so the last is after the phases.
fn read_window(d: &RunData, name: &str) -> Window {
	let times = d.times(name);
	let (start, end) = times
		.iter()
		.fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), t| (a.min(*t), b.max(*t)));
	Window { start, end, label: "read".into() }
}

fn calls(d: &RunData, w: &Window, call: &str, result: &str) -> Result<f64, CounterReset> {
	Ok(d.diff("polkameter_maintenance_calls_total", &[("call", call), ("result", result)], w)?
		.unwrap_or(0.0))
}

fn backlog_clears(d: &RunData) -> Result<Verdict, CounterReset> {
	if !d.has("polkameter_recycler_queued_keys", &[]) {
		return Ok(Verdict::new(Status::NoResult, NO_DATA));
	}
	let w = read_window(d, "polkameter_recycler_queued_keys");
	let start = backlog(d, w.start);
	let end = backlog(d, w.end);
	let peak = d.gauge_range("polkameter_recycler_queued_keys", &[], &w).map_or(0.0, |r| r.1);
	let done = d.phase("done").map_or(d.run().end, |p| p.start);
	let mut times: Vec<f64> = d
		.times("polkameter_recycler_unbuilt_keys")
		.into_iter()
		.filter(|t| *t >= done)
		.collect();
	times.sort_by(f64::total_cmp);
	let clear_at = times.iter().copied().find(|t| {
		let b = backlog(d, *t);
		b.0 + b.1 <= start.0 + start.1
	});
	let grew = end.0 > start.0 || end.1 > start.1 || end.2 > start.2;
	let mut detail = format!(
		"at the end: {} queued, {} unbuilt, {} stale rings (start {} / {} / {}); queue peak {}",
		num(end.0),
		num(end.1),
		num(end.2),
		num(start.0),
		num(start.1),
		num(start.2),
		num(peak)
	);
	if let Some(t) = clear_at {
		detail += &format!(
			"; back to the start {} s after the last load",
			num(((t - done) / 1000.0).round())
		);
	}
	Ok(Verdict::new(if grew { Status::Fail } else { Status::Pass }, detail))
}

fn maintenance_keeps_up(d: &RunData) -> Result<Verdict, CounterReset> {
	if !d.has("polkameter_recycler_stale_rings", &[]) {
		return Ok(Verdict::new(Status::NoResult, NO_DATA));
	}
	let w = read_window(d, "polkameter_recycler_stale_rings");
	let stale_peak = d.gauge_range("polkameter_recycler_stale_rings", &[], &w).map_or(0.0, |r| r.1);
	let builds = calls(d, &w, "Members.build_ring_authorized", "success")?;
	let onboards = calls(d, &w, "Members.onboard_members_authorized", "success")?;
	let failed = d
		.diff("polkameter_maintenance_calls_total", &[("result", "failed")], &w)?
		.unwrap_or(0.0);
	let starved = stale_peak > 0.0 && builds == 0.0;
	let detail = format!(
		"{} ring builds, {} onboardings, {} failed maintenance calls; stale rings peak {}",
		num(builds),
		num(onboards),
		num(failed),
		num(stale_peak)
	);
	Ok(Verdict::new(if starved || failed > 0.0 { Status::Fail } else { Status::Pass }, detail))
}

fn cleanup_keeps_up(d: &RunData) -> Result<Verdict, CounterReset> {
	if !d.has("polkameter_cleanup_backlog", &[]) {
		return Ok(Verdict::new(Status::NoResult, NO_DATA));
	}
	let w = read_window(d, "polkameter_cleanup_backlog");
	let kinds: Vec<(&str, f64, f64)> = ["ring_pages", "old_roots", "suspensions"]
		.into_iter()
		.map(|kind| {
			(
				kind,
				sum(d, "polkameter_cleanup_backlog", w.start, &[("kind", kind)]),
				sum(d, "polkameter_cleanup_backlog", w.end, &[("kind", kind)]),
			)
		})
		.collect();
	// Old roots are kept for 600 s, so a run shorter than that ends with more of them.
	let short = w.end - w.start < 600_000.0;
	let grew = kinds
		.iter()
		.any(|(kind, start, end)| end > start && !(short && *kind == "old_roots"));
	let mut detail = kinds
		.iter()
		.map(|(kind, start, end)| format!("{kind} {} -> {}", num(*start), num(*end)))
		.collect::<Vec<_>>()
		.join(", ");
	if short {
		detail += " (old roots are kept 600 s, longer than this run)";
	}
	Ok(Verdict::new(if grew { Status::Warn } else { Status::Pass }, detail))
}

fn load_to_root(d: &RunData) -> Result<Verdict, CounterReset> {
	// Up to the last observation: vouchers keep reaching a root while the run waits for the Recycler.
	let w = Window { start: d.run().start, end: f64::INFINITY, label: "run".into() };
	let b = d.buckets("polkameter_voucher_in_root_seconds", &[], &w)?;
	let n = b
		.as_ref()
		.and_then(|b| b.iter().find(|(le, _)| le.is_infinite()))
		.map_or(0.0, |x| x.1);
	if n == 0.0 {
		return Ok(Verdict::new(
			Status::NoResult,
			"no sampled voucher reached a built root (no loads in this run?)",
		));
	}
	let q = |q: f64| quantile(b.as_ref(), q).map_or("-".into(), num);
	Ok(Verdict::new(
		Status::Info,
		format!("{} sampled vouchers: p50 {} s, p95 {} s (bucket bounds)", num(n), q(0.5), q(0.95)),
	))
}

/// The Recycler checks.
pub const CHECKS: &[Check] = &[
	Check {
		outcome: Outcome::Recycler,
		name: "the backlog clears",
		optional: false,
		run: backlog_clears,
	},
	Check {
		outcome: Outcome::Recycler,
		name: "maintenance keeps up",
		optional: false,
		run: maintenance_keeps_up,
	},
	Check {
		outcome: Outcome::Recycler,
		name: "cleanup keeps up",
		optional: false,
		run: cleanup_keeps_up,
	},
	Check {
		outcome: Outcome::Recycler,
		name: "time from load to built root",
		optional: true,
		run: load_to_root,
	},
];
