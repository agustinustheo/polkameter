//! PVF (outcomes.md): level 1 counts the observed parachain's relay slots on chain and decides
//! pass or fail; level 2 reads the validators (all parachains together) for timing and causes.

use polkameter_plugin_sdk::observers::{
	CounterReset, Label, Phase, RunData, Status, Verdict, Window, count_above, num, quantile,
	to_fixed,
};
use serde::Serialize;

use crate::engine::checks::{
	COLLATOR, Check, Outcome,
	limits::LIMITS,
	outcomes::{also_missing, failed_scrapes, gaps, long_enough, no_data},
};

const VALIDATOR: (&str, &str) = ("job", "validator");
const FUNNEL: (&str, &str) = ("job", "collator-relay");
/// The relay chain's block interval: a window of two or more must see a PVF execution while
/// parachains run.
const RELAY_BLOCK_S: f64 = 6.0;

/// The `para` label of the observed parachain; empty when the run declared none, so its series
/// are absent and the checks have no result.
fn para(d: &RunData) -> String {
	d.summary.network.para_id.map(|id| id.to_string()).unwrap_or_default()
}

/// Where the parachain's offered slots went in a window; the parts add up to `missed`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Slots {
	/// Finalized relay blocks the recorder read.
	pub relay_blocks: f64,
	/// Slots offered to the parachain.
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

/// The parachain's slots in `w`.
fn slots(d: &RunData, w: &Window) -> Result<Slots, CounterReset> {
	let para = para(d);
	let para = ("para", para.as_str());
	let increase = |name: &str, filter: &[(&str, &str)]| d.increase(name, filter, w);
	let offered = increase("polkameter_para_slots_total", &[para])?;
	let included = increase("polkameter_para_included_total", &[para])?;
	let timed_out = increase("polkameter_para_timed_out_total", &[para])?;
	let relay_blocks = increase("polkameter_relay_finalized_blocks_total", &[])?;
	let built = increase("substrate_proposer_block_constructed_count", &[COLLATOR])?;
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
	window: Label,
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

/// The recorder counts a relay block, and the slots in it, when it reads the block after
/// finality, so finality lag moves both into a later window together. A window without relay
/// blocks is therefore not missing data; only a run in which the recorder read none is.
fn relay_slots(d: &RunData) -> Result<Verdict, CounterReset> {
	let para = para(d);
	let (Some(base), true) = (
		d.phase(Phase::Baseline),
		d.has("polkameter_para_slots_total", &[("para", para.as_str())]),
	) else {
		return Ok(Verdict::new(Status::NoResult, "no relay recorder data"));
	};
	let idle = slots(d, &base)?;
	// Without a relay block in the baseline there is no idle rate: every missed slot counts.
	let idle_rate = if idle.relay_blocks > 0.0 { idle.missed / idle.relay_blocks } else { 0.0 };
	let mut windows = Vec::new();
	let mut missing = Vec::new();
	for w in d.load_windows() {
		match slots(d, &w) {
			Ok(s) => windows.push(WindowSlots {
				window: w.label,
				extra: s.missed - idle_rate * s.relay_blocks,
				slots: s,
			}),
			Err(reset) => missing.push(format!("{}: a node restarted: {reset}", w.label)),
		}
	}
	let numbers = serde_json::json!({ "idle": idle, "windows": windows });
	let split = |s: &Slots| {
		format!(
			"not built {} (collator), not backed {} (PVF, delivery or statements; forks not measured yet), timed out {} (availability)",
			num(s.not_built),
			num(s.not_backed),
			num(s.timed_out)
		)
	};
	if let Some(b) = worst_violation(&windows) {
		let detail = format!(
			"{}: missed {} of {} slots, {} more than at idle: {}{}",
			b.window,
			num(b.slots.missed),
			num(b.slots.offered),
			to_fixed(b.extra, 1),
			split(&b.slots),
			also_missing(&missing)
		);
		return Ok(Verdict::new(Status::Fail, detail).with(numbers));
	}
	if !missing.is_empty() {
		return Ok(no_data(&missing, "").with(numbers));
	}
	let relay_blocks =
		idle.relay_blocks + windows.iter().map(|s| s.slots.relay_blocks).sum::<f64>();
	if relay_blocks == 0.0 {
		return Ok(Verdict::new(
			Status::NoResult,
			"the relay recorder read no finalized relay block in the run",
		)
		.with(numbers));
	}
	if idle.offered + windows.iter().map(|s| s.slots.offered).sum::<f64>() == 0.0 {
		return Ok(Verdict::new(
			Status::NoResult,
			format!("no relay slot was offered to parachain {para}"),
		)
		.with(numbers));
	}
	let idle_note = if idle.relay_blocks > 0.0 {
		String::new()
	} else {
		"; the baseline read no finalized relay block, so idle counts as no missed slot".into()
	};
	let detail = format!(
		"every step and recovery within {} missed slot of idle (idle: {} of {}){idle_note}",
		LIMITS.max_extra_missed_slots,
		num(idle.missed),
		num(idle.offered)
	);
	Ok(Verdict::new(Status::Pass, detail).with(numbers))
}

fn no_timeouts_or_disputes(d: &RunData) -> Result<Verdict, CounterReset> {
	if !d.has("polkameter_relay_finalized_blocks_total", &[]) {
		return Ok(Verdict::new(Status::NoResult, "no relay recorder data"));
	}
	let w = d.run();
	let run = std::slice::from_ref(&w);
	let para = para(d);
	let timed_out =
		d.increase("polkameter_para_timed_out_total", &[("para", para.as_str())], &w)?;
	let relay = d.increase("polkameter_relay_dispute_total", &[], &w)?;
	let disputes = "polkadot_parachain_candidate_disputes_total";
	let mut missing: Vec<String> =
		gaps(d, disputes, &[VALIDATOR], run).into_iter().flatten().collect();
	let raised = match d.diff(disputes, &[VALIDATOR], &w) {
		Ok(raised) if missing.is_empty() => raised,
		Ok(_) => None,
		Err(reset) => {
			missing.push(format!("run: a node restarted: {reset}"));
			None
		},
	};
	let note = failed_scrapes(d, &[VALIDATOR], run);
	let detail = format!(
		"parachain candidates timed out {}; disputes on the relay {}, raised by validators {}{}{note}",
		num(timed_out),
		num(relay),
		raised.map_or("-".into(), num),
		also_missing(&missing)
	);
	if timed_out + relay + raised.unwrap_or(0.0) > 0.0 {
		return Ok(Verdict::new(Status::Fail, detail));
	}
	if !missing.is_empty() {
		return Ok(no_data(&missing, &note));
	}
	if d.increase("polkameter_relay_finalized_blocks_total", &[], &w)? == 0.0 {
		return Ok(Verdict::new(
			Status::NoResult,
			"the relay recorder read no finalized relay block in the run",
		));
	}
	Ok(Verdict::new(Status::Pass, detail))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PvfWindow {
	window: Label,
	runs: f64,
	over2s: f64,
	p95_s: Option<f64>,
	queue_p95_s: Option<f64>,
	invalid: f64,
}

/// One window's PVF numbers.
fn pvf_window(d: &RunData, w: &Window) -> Result<PvfWindow, CounterReset> {
	let exec = d.buckets("polkadot_pvf_execution_time", &[VALIDATOR], w)?;
	let queued = d.buckets("polkadot_pvf_execution_queued_time", &[VALIDATOR], w)?;
	Ok(PvfWindow {
		runs: exec
			.as_ref()
			.and_then(|b| b.iter().find(|(le, _)| le.is_infinite()))
			.map_or(0.0, |x| x.1),
		over2s: count_above(exec.as_ref(), LIMITS.backing_timeout_s).unwrap_or(0.0),
		p95_s: quantile(exec.as_ref(), 0.95),
		queue_p95_s: quantile(queued.as_ref(), 0.95),
		invalid: d.increase(
			"polkadot_parachain_validation_requests_total",
			&[VALIDATOR, ("validity", "invalid")],
			w,
		)?,
		window: w.label,
	})
}

fn pvf_time(d: &RunData) -> Result<Verdict, CounterReset> {
	let bucket = "polkadot_pvf_execution_time_bucket";
	if !d.has(bucket, &[VALIDATOR]) {
		return Ok(Verdict::new(Status::NoResult, "no validator metrics"));
	}
	let timeout = LIMITS.backing_timeout_s;
	let load = d.load_windows();
	let note = failed_scrapes(d, &[VALIDATOR], &load);
	let mut windows = Vec::new();
	let mut missing = Vec::new();
	for (w, gap) in load.iter().zip(gaps(d, bucket, &[VALIDATOR], &load)) {
		if let Some(gap) = gap {
			missing.push(gap);
			continue;
		}
		match pvf_window(d, w) {
			Ok(p) if p.runs == 0.0 && long_enough(w, RELAY_BLOCK_S) => {
				missing.push(format!("{}: the validators recorded no PVF execution", w.label))
			},
			Ok(p) => windows.push(p),
			Err(reset) => missing.push(format!("{}: a node restarted: {reset}", w.label)),
		}
	}
	let numbers = serde_json::json!({ "windows": windows });
	let slow: Vec<String> = windows
		.iter()
		.filter(|s| s.over2s > 0.0 || s.invalid > 0.0)
		.map(|s| s.window.to_string())
		.collect();
	if !slow.is_empty() {
		let detail = format!(
			"{}: PVF runs over {} s or invalid validations (a timeout counts as invalid){}{note}",
			slow.join(", "),
			num(timeout),
			also_missing(&missing)
		);
		return Ok(Verdict::new(Status::Warn, detail).with(numbers));
	}
	if !missing.is_empty() {
		return Ok(no_data(&missing, &note).with(numbers));
	}
	if windows.iter().all(|w| w.runs == 0.0) {
		let none = ["the validators recorded no PVF execution in any window".to_owned()];
		return Ok(no_data(&none, &note).with(numbers));
	}
	let mut by_p95: Vec<&PvfWindow> = windows.iter().collect();
	by_p95.sort_by(|a, b| b.p95_s.unwrap_or(0.0).total_cmp(&a.p95_s.unwrap_or(0.0)));
	let worst = by_p95.first();
	let detail = format!(
		"no run over {} s; highest p95 {} s in {}, queue p95 {} s{note}",
		num(timeout),
		worst.and_then(|w| w.p95_s).map_or("-".into(), num),
		worst.map_or("-".to_owned(), |w| w.window.to_string()),
		worst.and_then(|w| w.queue_p95_s).map_or("-".into(), num)
	);
	Ok(Verdict::new(Status::Pass, detail).with(numbers))
}

fn collation_funnel(d: &RunData) -> Result<Verdict, CounterReset> {
	let generated_total = "polkadot_parachain_collations_generated_total";
	if !d.has(generated_total, &[FUNNEL]) {
		return Ok(Verdict::new(Status::NoResult, "no metrics from the collator's relay node"));
	}
	let w = d.run();
	let run = std::slice::from_ref(&w);
	let missing: Vec<String> =
		gaps(d, generated_total, &[FUNNEL], run).into_iter().flatten().collect();
	let note = failed_scrapes(d, &[FUNNEL], run);
	let generated = d.diff(generated_total, &[FUNNEL], &w)?;
	let expired = |state: &str| {
		d.increase("polkadot_parachain_collation_expired_count", &[FUNNEL, ("state", state)], &w)
	};
	let (advertised, fetched, backed) =
		(expired("advertised")?, expired("fetched")?, expired("backed")?);
	let inc_n =
		d.increase("polkadot_parachain_collation_inclusion_latency_count", &[FUNNEL], &w)?;
	let inc_sum =
		d.increase("polkadot_parachain_collation_inclusion_latency_sum", &[FUNNEL], &w)?;
	let latency = if inc_n > 0.0 { Some(inc_sum / inc_n) } else { None };
	// An expired collation was counted, whatever the edges; a pass needs fresh edges.
	let any = advertised + fetched + backed > 0.0;
	let warn = any || latency.unwrap_or(1.0) > 1.5;
	let numbers = serde_json::json!({ "generated": generated, "lost": { "advertised": advertised, "fetched": fetched, "backed": backed }, "inclusionLatency": latency });
	if !warn && !missing.is_empty() {
		return Ok(no_data(&missing, &note).with(numbers));
	}
	if !warn && generated.unwrap_or(0.0) == 0.0 {
		let none = ["the collator's relay node recorded no generated collation".to_owned()];
		return Ok(no_data(&none, &note).with(numbers));
	}
	let detail = format!(
		"expired after advertised {} (delivery), after fetched {} (validation or backing), after backed {} (availability); backed to included {} relay blocks on average{}{note}",
		num(advertised),
		num(fetched),
		num(backed),
		latency.map_or("-".into(), |l| to_fixed(l, 2)),
		also_missing(&missing)
	);
	let status = if warn { Status::Warn } else { Status::Pass };
	Ok(Verdict::new(status, detail).with(numbers))
}

/// The PVF checks.
pub const CHECKS: &[Check] = &[
	Check {
		outcome: Outcome::Pvf,
		name: "relay slots (level 1)",
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
		name: "collation funnel",
		optional: false,
		run: collation_funnel,
	},
];
