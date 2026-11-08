//! Every rule of the ramp, in one place (the README's "Phases and rules" is the same list).
//!
//! Response measures. The first one a step violates is the breaking point, and the ramp goes on:
//!   included   < 90% of a step's txs included
//!   latency    p95 send -> best block > 10 s
//!   refused    > 1% of a step's submits refused
//!
//! Failures. The first one ends the ramp:
//!   pool refuses    > 10% of a step's submits refused                         graceful
//!   pool intake     p95 submit reply > 5 s, or the node stops reading submits graceful
//!   slow blocks     blocks arrive 2x slower than at the start (wall clock)    hard
//!   stall           no new best block for 30 s, while the follower is connected hard
//!   finality stall  no new finalized block for 60 s, same                      hard
//!   node down       the node stayed unreachable after every reconnect attempt  hard
//!
//! A dropped RPC connection that opens again is an observation gap (a problem), not a failure:
//! the stall rules count from the reconnect.
//!
//! The ramp also ends without a failure on: rate cap, budget used up, generator limit, smoke error.

use polkameter_plugin_sdk::observers::files::{
	FinalStep, Millis,
	summary::{BreakingPoint, Class, FailureMode, Loss, Rule, Stop},
	to_fixed,
};
use serde::Serialize;

use crate::load::{
	source::QueueSource,
	steps::{StepStats, final_step},
};

/// The thresholds.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
#[allow(missing_docs)]
pub struct Rules {
	pub min_included_ratio: f64,
	pub max_p95_latency_ms: Millis,
	pub max_refused_ratio: f64,
	pub pool_refuses_ratio: f64,
	pub max_submit_reply_ms: Millis,
	pub max_block_gap_factor: f64,
	pub stall_ms: Millis,
	pub finality_stall_ms: Millis,
	pub min_send_ratio: f64,
	/// A send tick later than this means the load tool itself was paused.
	pub max_tick_gap_ms: Millis,
	pub probes_in_a_row: usize,
	pub recovered_block_gap_factor: f64,
	pub finality_wait_ms: Millis,
}

/// The rules.
pub const RULES: Rules = Rules {
	min_included_ratio: 0.9,
	max_p95_latency_ms: 10_000,
	max_refused_ratio: 0.01,
	pool_refuses_ratio: 0.1,
	max_submit_reply_ms: 5_000,
	max_block_gap_factor: 2.0,
	stall_ms: 30_000,
	finality_stall_ms: 60_000,
	min_send_ratio: 0.95,
	max_tick_gap_ms: 5_000,
	probes_in_a_row: 3,
	recovered_block_gap_factor: 1.5,
	finality_wait_ms: 180_000,
};

fn pct(ratio: f64) -> String {
	format!("{}%", to_fixed(ratio * 100.0, 1))
}

/// Seconds, one decimal, rounded as the TS tool rounds.
pub fn secs(ms: impl Into<f64>) -> String {
	to_fixed(ms.into() / 1000.0, 1)
}

/// The first response measure a finished step violates: (measure, detail).
pub fn measure_violation(f: &FinalStep, rules: &Rules) -> Option<(&'static str, String)> {
	if f.sent == 0 {
		return None;
	}
	if f.included_ratio < rules.min_included_ratio {
		return Some(("included", format!("{} of the step's txs included", pct(f.included_ratio))));
	}
	if f.p95_latency_ms > rules.max_p95_latency_ms {
		return Some((
			"latency",
			format!("p95 send -> best block {} s", secs(f.p95_latency_ms as f64)),
		));
	}
	if f.rejected_ratio > rules.max_refused_ratio {
		return Some(("refused", format!("{} of submits refused", pct(f.rejected_ratio))));
	}
	None
}

/// What the ramp checks on every send tick.
#[derive(Debug, Clone, Default)]
pub struct LiveState {
	/// The step.
	pub step: u32,
	/// Now.
	pub now: Millis,
	/// Connections the node closed.
	pub closed_by_node: usize,
	/// Connections opened.
	pub connections: usize,
	/// Last new best block.
	pub last_block_at: Millis,
	/// Last new finalized block.
	pub last_finalized_at: Millis,
	/// Best number.
	pub best: u32,
	/// Finalized number.
	pub finalized: u32,
	/// Monitor problems, in smoke mode only.
	pub smoke_problems: Vec<String>,
	/// Time since the last new best block that the block follower was disconnected: left out
	/// of the stall rule, which no block could be seen in.
	pub unobserved_since_block: Millis,
	/// The same, since the last new finalized block.
	pub unobserved_since_finalized: Millis,
	/// The node is down: unreachable for the reconnect backoff, dropping the connection again
	/// and again, or restarted.
	pub node_down: Option<String>,
}

/// A failure that shows during a step.
pub fn live_failure(s: &LiveState, rules: &Rules) -> Option<Stop> {
	let ago = |t: Millis| (s.now.saturating_sub(t) as f64 / 1000.0).round();
	let step = Some(s.step);
	if s.closed_by_node > 0 {
		return Some(Stop::new(
			Rule::NodeDown,
			step,
			format!(
				"{} of {} RPC connections dropped and could not be opened again",
				s.closed_by_node, s.connections
			),
		));
	}
	if let Some(why) = &s.node_down {
		return Some(Stop::new(Rule::NodeDown, step, why.clone()));
	}
	// Observed time only: a reconnect neither hides a stall nor makes one.
	let since_block =
		s.now.saturating_sub(s.last_block_at).saturating_sub(s.unobserved_since_block);
	let since_finalized = s
		.now
		.saturating_sub(s.last_finalized_at)
		.saturating_sub(s.unobserved_since_finalized);
	if since_block > rules.stall_ms {
		return Some(Stop::new(
			Rule::Stall,
			step,
			format!(
				"no new best block for {} s{}",
				ago(s.last_block_at),
				not_observed(s.unobserved_since_block)
			),
		));
	}
	if since_finalized > rules.finality_stall_ms {
		let detail = format!(
			"no new finalized block for {} s{} (best {}, finalized {})",
			ago(s.last_finalized_at),
			not_observed(s.unobserved_since_finalized),
			s.best,
			s.finalized
		);
		return Some(Stop::new(Rule::FinalityStall, step, detail));
	}
	(!s.smoke_problems.is_empty())
		.then(|| Stop::new(Rule::SmokeError, step, s.smoke_problems.join("; ")))
}

/// ", N s of it while reconnecting" for a stall detail.
fn not_observed(ms: Millis) -> String {
	if ms == 0 {
		String::new()
	} else {
		format!(", {} s of it while reconnecting", secs(ms as f64))
	}
}

/// The rules checked at the end of each step for one lane, on it and on the step before (settled by now).
///
/// `last` is true after the plan's last step. A flood that is empty only then was sized to the
/// plan: every step ran, so that is the rate cap, not budget used up. A flood that ran out during
/// the last step still is budget used up when the step sent too little.
pub fn check_step(
	steps: &[StepStats],
	source: &QueueSource,
	unsent_kib: u64,
	block_ms: f64,
	rules: &Rules,
	last: bool,
) -> Option<Stop> {
	let cur = final_step(steps.last()?);
	let prev = steps.len().checked_sub(2).map(|i| final_step(&steps[i]));
	for f in prev.iter().chain([&cur]) {
		if f.sent > 0 && f.rejected_ratio > rules.pool_refuses_ratio {
			let top = f.errors.iter().max_by_key(|(_, n)| **n).map_or("?", |(k, _)| k.as_str());
			return Some(Stop::new(
				Rule::PoolRefuses,
				Some(f.step),
				format!(
					"{} of step {}'s submits refused, mostly \"{top}\"",
					pct(f.rejected_ratio),
					f.step
				),
			));
		}
	}
	let step = Some(cur.step);
	if cur.p95_reply_ms > rules.max_submit_reply_ms {
		return Some(Stop::new(
			Rule::PoolIntake,
			step,
			format!(
				"p95 submit reply {} s: the node takes that long to check and accept a tx",
				secs(cur.p95_reply_ms as f64)
			),
		));
	}
	if cur.max_oldest_pending_ms > rules.max_submit_reply_ms {
		return Some(Stop::new(
			Rule::PoolIntake,
			step,
			format!("a submit waited {} s for a reply", secs(cur.max_oldest_pending_ms as f64)),
		));
	}
	let slow_send = cur.send_ratio < rules.min_send_ratio;
	if slow_send && cur.backpressure_ticks > 0 {
		let detail = format!(
			"the node stopped reading submits ({unsent_kib} KiB unsent); sent {} of {} tx/s",
			to_fixed(cur.sent_per_s, 0),
			cur.target_rate
		);
		return Some(Stop::new(Rule::PoolIntake, step, detail));
	}
	if let Some(gap) = cur
		.blocks
		.mean_block_gap_ms
		.filter(|g| *g > rules.max_block_gap_factor * block_ms)
	{
		return Some(Stop::new(
			Rule::SlowBlocks,
			step,
			format!("a block every {} s, against {} s at the start", secs(gap), secs(block_ms)),
		));
	}
	if (!last && source.exhausted()) || (slow_send && cur.starved_ticks > 0) {
		return Some(Stop::new(Rule::BudgetUsedUp, step, source.starved_reason()));
	}
	slow_send.then(|| {
		Stop::new(
			Rule::GeneratorLimit,
			step,
			format!("sent {} of {} tx/s", to_fixed(cur.sent_per_s, 0), cur.target_rate),
		)
	})
}

/// From the final numbers, after recovery: the first step that violated a measure.
pub fn breaking_point(finals: &[FinalStep], rules: &Rules) -> Option<BreakingPoint> {
	finals.iter().find_map(|f| {
		let (measure, detail) = measure_violation(f, rules)?;
		Some(BreakingPoint {
			step: f.step,
			target_rate: f.target_rate,
			measure: measure.into(),
			detail,
		})
	})
}

/// The failure classes of a run: the load's stop, plus what the loss check found.
pub fn failure_modes(stop: &Stop, loss: &Loss, finals: &[FinalStep]) -> Vec<FailureMode> {
	let mut modes = Vec::new();
	if let Some(class) = stop.class {
		let rate = finals
			.iter()
			.find(|f| Some(f.step) == stop.step)
			.map_or(String::from("?"), |f| f.target_rate.to_string());
		modes.push(FailureMode { class, what: format!("{} at {rate} tx/s", stop.rule.name()) });
	}
	if loss.failed_in_block > 0 {
		modes.push(FailureMode {
			class: Class::Hard,
			what: format!("{} txs failed after inclusion", loss.failed_in_block),
		});
	}
	if let Some(lost) = loss.lost.filter(|l| *l > 0) {
		let stuck = loss.node_pool.as_ref().filter(|p| p.mempool.saturating_sub(p.ready) >= lost);
		let what = match stuck {
			Some(p) => format!(
				"{lost} txs the node accepted are in no block and not ready; its mempool still holds {} txs with {} ready, so they look stuck there. No error came back",
				p.mempool, p.ready
			),
			None => format!(
				"{lost} txs the node accepted are in no block and not in the ready pool, and no error came back"
			),
		};
		modes.push(FailureMode { class: Class::Silent, what });
	}
	if let Some(s) = loss.state.as_ref().filter(|s| s.missing > 0) {
		modes.push(FailureMode {
			class: Class::Silent,
			what: format!("{} of {} included txs left no state", s.missing, s.checked),
		});
	}
	modes
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::load::source::Tx;

	/// A 10 s step at 100 tx/s that sent everything; `f` changes what a case needs.
	fn step(k: u32, f: impl FnOnce(&mut StepStats)) -> StepStats {
		let mut st = StepStats {
			step: k,
			target_rate: 100.0,
			started_at: 0,
			ended_at: 10_000,
			sent: 1_000,
			included: 1_000,
			latencies_ms: vec![1_000],
			reply_ms: vec![10],
			..Default::default()
		};
		f(&mut st);
		st
	}

	fn source() -> QueueSource {
		QueueSource::new(vec![Tx::new(vec![1])], vec![], "coins")
	}

	#[test]
	fn hard_rules_name_the_violated_measure() {
		for (changed, expected) in [
			(step(0, |_| ()), None),
			(step(0, |s| s.included = 800), Some("included")),
			(step(0, |s| s.latencies_ms = vec![11_000]), Some("latency")),
			(step(0, |s| s.rejected = 20), Some("refused")),
		] {
			let violation = measure_violation(&final_step(&changed), &RULES);
			assert_eq!(violation.map(|v| v.0), expected);
		}
		// Pool refusals in the step before the last fail it gracefully.
		let steps = [
			step(0, |s| {
				s.rejected = 200;
				s.errors.insert("1010 Immediately Dropped".into(), 200);
			}),
			step(1, |_| ()),
		];
		let stop = check_step(&steps, &source(), 0, 6_000.0, &RULES, false).unwrap();
		assert_eq!(
			(stop.rule, stop.step, stop.class),
			(Rule::PoolRefuses, Some(0), Some(Class::Graceful))
		);
	}

	fn live() -> LiveState {
		LiveState {
			now: 100_000,
			connections: 4,
			last_block_at: 60_000,
			last_finalized_at: 90_000,
			..Default::default()
		}
	}

	#[test]
	fn slow_blocks_and_stalls_count_only_observed_time() {
		// 10 s with one new best block against 4 s at the start; 20 s with none against 6 s.
		for (slow, start_gap_ms) in [
			(
				step(0, |s| {
					s.heads = 1;
					s.closed = true;
				}),
				4_000.0,
			),
			(
				step(0, |s| {
					s.ended_at = 20_000;
					s.closed = true;
				}),
				6_000.0,
			),
		] {
			let stop = check_step(&[slow], &source(), 0, start_gap_ms, &RULES, false).unwrap();
			assert_eq!((stop.rule, stop.class), (Rule::SlowBlocks, Some(Class::Hard)));
		}
		// A step not observed at all has nothing to judge.
		let unobserved = step(0, |s| {
			s.ended_at = 20_000;
			s.closed = true;
			s.unobserved_ms = 20_000;
		});
		assert_eq!(final_step(&unobserved).blocks.mean_block_gap_ms, None);
		// 40 s since the last block; time spent reconnecting does not count.
		let down = Some("blocks: the node did not answer for 15 s".to_owned());
		for (state, expected) in [
			(live(), Some(Rule::Stall)),
			(LiveState { unobserved_since_block: 15_000, ..live() }, None),
			(LiveState { unobserved_since_block: 5_000, ..live() }, Some(Rule::Stall)),
			(LiveState { node_down: down, ..live() }, Some(Rule::NodeDown)),
		] {
			let stop = live_failure(&state, &RULES);
			assert_eq!(stop.as_ref().map(|stop| stop.rule), expected);
			if let Some(stop) = stop {
				assert_eq!(stop.class, Some(Class::Hard));
			}
		}
	}

	#[test]
	fn a_load_tool_that_cannot_keep_up_ends_without_a_failure() {
		let short = |starved_ticks| {
			step(0, |s| {
				s.sent = 500;
				s.included = 500;
				s.starved_ticks = starved_ticks;
			})
		};
		// Every tx sent at the full rate: the flood is empty at the end of the step.
		let empty = || QueueSource::new(vec![], vec![], "coins");
		assert!(empty().exhausted());
		for (steps, source, last, expected) in [
			(short(0), source(), false, Some(Rule::GeneratorLimit)),
			(short(3), source(), false, Some(Rule::BudgetUsedUp)),
			// A later step would have nothing to send.
			(step(0, |_| ()), empty(), false, Some(Rule::BudgetUsedUp)),
			(step(0, |_| ()), empty(), true, None),
			// Ran out during the last step and sent too little.
			(short(3), empty(), true, Some(Rule::BudgetUsedUp)),
		] {
			let stop = check_step(&[steps], &source, 0, 6_000.0, &RULES, last);
			assert_eq!(stop.as_ref().map(|stop| stop.rule), expected);
			if let Some(stop) = stop {
				assert_eq!(stop.class, None, "{:?}", stop.rule);
			}
		}
	}
}
