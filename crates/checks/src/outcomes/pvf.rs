//! PVF (outcomes.md): level 1 counts People's relay slots on chain and decides pass or fail;
//! level 2 reads the validators (all parachains together) for timing and causes.

use polkameter_files::registry::Outcome;
use polkameter_files::{PEOPLE_PARA_ID, num, to_fixed};
use serde::Serialize;

use crate::data::{CounterReset, RunData, Window, count_above, quantile};
use crate::{Check, LIMITS, Status, Verdict};

const PEOPLE: (&str, &str) = ("para", PEOPLE_PARA_ID);
const VALIDATOR: (&str, &str) = ("job", "validator");
const FUNNEL: (&str, &str) = ("job", "people-collator-relay");

/// Where People's offered slots went in a window; the parts add up to `missed`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Slots {
	/// Finalized relay blocks the recorder read.
	pub relay_blocks: f64,
	/// Slots offered to People.
	pub offered: f64,
	/// Candidates included.
	pub included: f64,
	/// Offered minus included.
	pub missed: f64,
	/// The collator built no block.
	pub not_built: f64,
	/// Backed but not available in time.
	pub timed_out: f64,
	/// The rest: PVF, delivery or statements.
	pub not_backed: f64,
}

/// People's slots in `w`.
fn slots(d: &RunData, w: &Window) -> Result<Slots, CounterReset> {
	let diff =
		|name: &str, filter: &[(&str, &str)]| d.diff(name, filter, w).map(|v| v.unwrap_or(0.0));
	let offered = diff("polkameter_para_slots_total", &[PEOPLE])?;
	let included = diff("polkameter_para_included_total", &[PEOPLE])?;
	let timed_out = diff("polkameter_para_timed_out_total", &[PEOPLE])?;
	let relay_blocks = diff("polkameter_relay_finalized_blocks_total", &[])?;
	let built = diff("substrate_proposer_block_constructed_count", &[("job", "people-collator")])?;
	let missed = (offered - included).max(0.0);
	let not_built = missed.min((offered - built).max(0.0));
	Ok(Slots {
		relay_blocks,
		offered,
		included,
		missed,
		not_built,
		timed_out,
		not_backed: (missed - not_built - timed_out).max(0.0),
	})
}

#[derive(Serialize)]
struct WindowSlots {
	window: String,
	#[serde(flatten)]
	slots: Slots,
	extra: f64,
}

fn worst_violation(windows: &[WindowSlots]) -> Option<&WindowSlots> {
	windows
		.iter()
		.filter(|s| s.extra > f64::from(LIMITS.max_extra_missed_slots))
		.max_by(|a, b| a.extra.total_cmp(&b.extra))
}

fn relay_slots(d: &RunData) -> Result<Verdict, CounterReset> {
	let (Some(base), true) = (d.phase("baseline"), d.has("polkameter_para_slots_total", &[PEOPLE]))
	else {
		return Ok(Verdict::new(Status::NoResult, "no relay recorder data"));
	};
	let idle = slots(d, &base)?;
	let idle_rate = if idle.relay_blocks > 0.0 { idle.missed / idle.relay_blocks } else { 0.0 };
	let windows = d
		.load_windows()
		.into_iter()
		.map(|w| {
			let s = slots(d, &w)?;
			Ok(WindowSlots {
				window: w.label,
				extra: s.missed - idle_rate * s.relay_blocks,
				slots: s,
			})
		})
		.collect::<Result<Vec<_>, CounterReset>>()?;
	let bad = worst_violation(&windows);
	let split = |s: &Slots| {
		format!(
			"not built {} (collator), not backed {} (PVF, delivery or statements; forks not measured yet), timed out {} (availability)",
			num(s.not_built),
			num(s.not_backed),
			num(s.timed_out)
		)
	};
	let detail = match bad {
		Some(b) => format!(
			"{}: missed {} of {} slots, {} more than at idle: {}",
			b.window,
			num(b.slots.missed),
			num(b.slots.offered),
			to_fixed(b.extra, 1),
			split(&b.slots)
		),
		None => format!(
			"every step and recovery within {} missed slot of idle (idle: {} of {})",
			LIMITS.max_extra_missed_slots,
			num(idle.missed),
			num(idle.offered)
		),
	};
	let status = if bad.is_some() { Status::Fail } else { Status::Pass };
	Ok(Verdict::new(status, detail).with(serde_json::json!({ "idle": idle, "windows": windows })))
}

fn no_timeouts_or_disputes(d: &RunData) -> Result<Verdict, CounterReset> {
	let w = d.run();
	let timed_out = d.diff("polkameter_para_timed_out_total", &[PEOPLE], &w)?.unwrap_or(0.0);
	let relay = d.diff("polkameter_relay_dispute_total", &[], &w)?.unwrap_or(0.0);
	let raised = d.diff("polkadot_parachain_candidate_disputes_total", &[VALIDATOR], &w)?;
	if !d.has("polkameter_relay_finalized_blocks_total", &[]) {
		return Ok(Verdict::new(Status::NoResult, "no relay recorder data"));
	}
	let bad = timed_out + relay + raised.unwrap_or(0.0) > 0.0;
	let detail = format!(
		"People candidates timed out {}; disputes on the relay {}, raised by validators {}",
		num(timed_out),
		num(relay),
		raised.map_or("-".into(), num)
	);
	Ok(Verdict::new(if bad { Status::Fail } else { Status::Pass }, detail))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PvfWindow {
	window: String,
	runs: f64,
	over2s: f64,
	p95_s: Option<f64>,
	queue_p95_s: Option<f64>,
	invalid: f64,
}

fn pvf_time(d: &RunData) -> Result<Verdict, CounterReset> {
	if !d.has("polkadot_pvf_execution_time_bucket", &[VALIDATOR]) {
		return Ok(Verdict::new(Status::NoResult, "no validator metrics"));
	}
	let timeout = LIMITS.backing_timeout_s;
	let mut windows = Vec::new();
	for w in d.load_windows() {
		let exec = d.buckets("polkadot_pvf_execution_time", &[VALIDATOR], &w)?;
		let queued = d.buckets("polkadot_pvf_execution_queued_time", &[VALIDATOR], &w)?;
		windows.push(PvfWindow {
			runs: exec
				.as_ref()
				.and_then(|b| b.iter().find(|(le, _)| le.is_infinite()))
				.map_or(0.0, |x| x.1),
			over2s: count_above(exec.as_ref(), timeout).unwrap_or(0.0),
			p95_s: quantile(exec.as_ref(), 0.95),
			queue_p95_s: quantile(queued.as_ref(), 0.95),
			invalid: d
				.diff(
					"polkadot_parachain_validation_requests_total",
					&[VALIDATOR, ("validity", "invalid")],
					&w,
				)?
				.unwrap_or(0.0),
			window: w.label,
		});
	}
	let slow: Vec<&str> = windows
		.iter()
		.filter(|s| s.over2s > 0.0 || s.invalid > 0.0)
		.map(|s| s.window.as_str())
		.collect();
	let mut by_p95: Vec<&PvfWindow> = windows.iter().collect();
	by_p95.sort_by(|a, b| b.p95_s.unwrap_or(0.0).total_cmp(&a.p95_s.unwrap_or(0.0)));
	let worst = by_p95.first();
	let detail = if slow.is_empty() {
		format!(
			"no run over {} s; highest p95 {} s in {}, queue p95 {} s",
			num(timeout),
			worst.and_then(|w| w.p95_s).map_or("-".into(), num),
			worst.map_or("-", |w| w.window.as_str()),
			worst.and_then(|w| w.queue_p95_s).map_or("-".into(), num)
		)
	} else {
		format!(
			"{}: PVF runs over {} s or invalid validations (a timeout counts as invalid)",
			slow.join(", "),
			num(timeout)
		)
	};
	let status = if slow.is_empty() { Status::Pass } else { Status::Warn };
	Ok(Verdict::new(status, detail).with(serde_json::json!({ "windows": windows })))
}

fn collation_funnel(d: &RunData) -> Result<Verdict, CounterReset> {
	if !d.has("polkadot_parachain_collations_generated_total", &[FUNNEL]) {
		return Ok(Verdict::new(Status::NoResult, "no metrics from the collator's relay node"));
	}
	let w = d.run();
	let expired = |state: &str| {
		d.diff("polkadot_parachain_collation_expired_count", &[FUNNEL, ("state", state)], &w)
			.map(|v| v.unwrap_or(0.0))
	};
	let (advertised, fetched, backed) =
		(expired("advertised")?, expired("fetched")?, expired("backed")?);
	let inc_n = d
		.diff("polkadot_parachain_collation_inclusion_latency_count", &[FUNNEL], &w)?
		.unwrap_or(0.0);
	let inc_sum = d
		.diff("polkadot_parachain_collation_inclusion_latency_sum", &[FUNNEL], &w)?
		.unwrap_or(0.0);
	let latency = if inc_n > 0.0 { Some(inc_sum / inc_n) } else { None };
	let any = advertised + fetched + backed > 0.0;
	let status = if any || latency.unwrap_or(1.0) > 1.5 { Status::Warn } else { Status::Pass };
	let detail = format!(
		"expired after advertised {} (delivery), after fetched {} (validation or backing), after backed {} (availability); backed to included {} relay blocks on average",
		num(advertised),
		num(fetched),
		num(backed),
		latency.map_or("-".into(), |l| to_fixed(l, 2))
	);
	let generated = d.diff("polkadot_parachain_collations_generated_total", &[FUNNEL], &w)?;
	Ok(Verdict::new(status, detail).with(serde_json::json!({ "generated": generated, "lost": { "advertised": advertised, "fetched": fetched, "backed": backed }, "inclusionLatency": latency })))
}

/// The PVF checks.
pub const CHECKS: &[Check] = &[
	Check {
		outcome: Outcome::Pvf,
		name: "People relay slots (level 1)",
		optional: false,
		run: relay_slots,
	},
	Check {
		outcome: Outcome::Pvf,
		name: "no timed-out candidates or disputes",
		optional: false,
		run: no_timeouts_or_disputes,
	},
	Check {
		outcome: Outcome::Pvf,
		name: "PVF time on validators (level 2, all parachains)",
		optional: false,
		run: pvf_time,
	},
	Check {
		outcome: Outcome::Pvf,
		name: "collation funnel (People)",
		optional: false,
		run: collation_funnel,
	},
];

#[cfg(test)]
mod tests {
	use super::*;

	fn window(name: &str, offered: f64, missed: f64) -> WindowSlots {
		WindowSlots {
			window: name.into(),
			slots: Slots {
				relay_blocks: 1.0,
				offered,
				included: offered - missed,
				missed,
				not_built: 0.0,
				timed_out: 0.0,
				not_backed: missed,
			},
			extra: missed,
		}
	}

	#[test]
	fn the_worst_violating_window_is_reported() {
		let windows = [window("step 1", 30.0, 3.0), window("recovery", 15.0, 9.0)];
		assert_eq!(worst_violation(&windows).unwrap().window, "recovery");
	}
}
