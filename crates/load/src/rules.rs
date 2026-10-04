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
//!   stall           no new best block for 30 s                                hard
//!   finality stall  no new finalized block for 60 s                           hard
//!   node down       the node closed an RPC connection                         hard
//!
//! The ramp also ends without a failure on: rate cap, budget used up, generator limit, smoke error.

use polkameter_files::summary::{BreakingPoint, Class, FailureMode, Loss, Rule, Stop};
use polkameter_files::to_fixed;
use polkameter_files::{FinalStep, Millis};
use serde::Serialize;

use crate::steps::{StepStats, final_step};

type Ms = Millis;

fn stop(rule: Rule, step: Option<u32>, detail: impl Into<String>) -> Stop {
	Stop::new(rule, step, detail)
}

/// The thresholds.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
#[allow(missing_docs)]
pub struct Rules {
	pub min_included_ratio: f64,
	pub max_p95_latency_ms: Ms,
	pub max_refused_ratio: f64,
	pub pool_refuses_ratio: f64,
	pub max_submit_reply_ms: Ms,
	pub max_block_gap_factor: f64,
	pub stall_ms: Ms,
	pub finality_stall_ms: Ms,
	pub min_send_ratio: f64,
	/// A send tick later than this means the load tool itself was paused.
	pub max_tick_gap_ms: Ms,
	pub probes_in_a_row: usize,
	pub recovered_block_gap_factor: f64,
	pub finality_wait_ms: Ms,
	pub recycler_drain_ms: Ms,
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
	recycler_drain_ms: 180_000,
};

fn pct(ratio: f64) -> String {
	format!("{}%", to_fixed(ratio * 100.0, 1))
}

/// Seconds, one decimal, rounded as the TS tool rounds.
pub fn secs(ms: impl Into<f64>) -> String {
	to_fixed(ms.into() / 1000.0, 1)
}

/// The first response measure a finished step violates: (measure, detail).
pub fn measure_violation_with_rules(
	f: &FinalStep,
	rules: &Rules,
) -> Option<(&'static str, String)> {
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
	pub now: Ms,
	/// Connections the node closed.
	pub closed_by_node: usize,
	/// Connections opened.
	pub connections: usize,
	/// Last new best block.
	pub last_block_at: Ms,
	/// Last new finalized block.
	pub last_finalized_at: Ms,
	/// Best number.
	pub best: u32,
	/// Finalized number.
	pub finalized: u32,
	/// Monitor problems, in smoke mode only.
	pub smoke_problems: Vec<String>,
}

/// A failure that shows during a step.
pub fn live_failure_with_rules(s: &LiveState, rules: &Rules) -> Option<Stop> {
	let ago = |t: Ms| (s.now.saturating_sub(t) as f64 / 1000.0).round();
	let step = Some(s.step);
	if s.closed_by_node > 0 {
		return Some(stop(
			Rule::NodeDown,
			step,
			format!("the node closed {} of {} RPC connections", s.closed_by_node, s.connections),
		));
	}
	if s.now.saturating_sub(s.last_block_at) > rules.stall_ms {
		return Some(stop(
			Rule::Stall,
			step,
			format!("no new best block for {} s", ago(s.last_block_at)),
		));
	}
	if s.now.saturating_sub(s.last_finalized_at) > rules.finality_stall_ms {
		let detail = format!(
			"no new finalized block for {} s (best {}, finalized {})",
			ago(s.last_finalized_at),
			s.best,
			s.finalized
		);
		return Some(stop(Rule::FinalityStall, step, detail));
	}
	(!s.smoke_problems.is_empty())
		.then(|| stop(Rule::SmokeError, step, s.smoke_problems.join("; ")))
}

/// What the step rules need to know about the source.
#[derive(Debug, Clone)]
pub struct SourceState {
	/// No tx left.
	pub exhausted: bool,
	/// Why, for the stop detail.
	pub starved_reason: String,
}

/// The rules checked at the end of each step for one lane, on it and on the step before (settled by now).
pub fn check_step_with_rules(
	steps: &[StepStats],
	source: &SourceState,
	unsent_kib: u64,
	block_ms: f64,
	rules: &Rules,
) -> Option<Stop> {
	let cur = final_step(steps.last()?, None);
	let prev = steps.len().checked_sub(2).map(|i| final_step(&steps[i], None));
	for f in prev.iter().chain([&cur]) {
		if f.sent > 0 && f.rejected_ratio > rules.pool_refuses_ratio {
			let top = f.errors.iter().max_by_key(|(_, n)| **n).map_or("?", |(k, _)| k.as_str());
			return Some(stop(
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
		return Some(stop(
			Rule::PoolIntake,
			step,
			format!(
				"p95 submit reply {} s: the node takes that long to check and accept a tx",
				secs(cur.p95_reply_ms as f64)
			),
		));
	}
	if cur.max_oldest_pending_ms > rules.max_submit_reply_ms {
		return Some(stop(
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
		return Some(stop(Rule::PoolIntake, step, detail));
	}
	if let Some(gap) = cur
		.blocks
		.mean_block_gap_ms
		.filter(|g| *g > rules.max_block_gap_factor * block_ms)
	{
		return Some(stop(
			Rule::SlowBlocks,
			step,
			format!("a block every {} s, against {} s at the start", secs(gap), secs(block_ms)),
		));
	}
	if source.exhausted || (slow_send && cur.starved_ticks > 0) {
		return Some(stop(Rule::BudgetUsedUp, step, source.starved_reason.clone()));
	}
	slow_send.then(|| {
		stop(
			Rule::GeneratorLimit,
			step,
			format!("sent {} of {} tx/s", to_fixed(cur.sent_per_s, 0), cur.target_rate),
		)
	})
}

/// From the final numbers, after recovery: the first step that violated a measure.
pub fn breaking_point_with_rules(finals: &[FinalStep], rules: &Rules) -> Option<BreakingPoint> {
	finals.iter().find_map(|f| {
		let (measure, detail) = measure_violation_with_rules(f, rules)?;
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
		modes.push(FailureMode {
			class,
			what: format!(
				"{} at {rate} tx/s",
				serde_json::to_value(stop.rule).expect("rule").as_str().unwrap_or("?")
			),
		});
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

	fn source() -> SourceState {
		SourceState { exhausted: false, starved_reason: "no coins".into() }
	}

	#[test]
	fn marks_the_first_violated_measure() {
		assert_eq!(measure_violation_with_rules(&final_step(&step(0, |_| ()), None), &RULES), None);
		assert_eq!(
			measure_violation_with_rules(&final_step(&step(0, |s| s.included = 800), None), &RULES)
				.unwrap()
				.0,
			"included"
		);
		assert_eq!(
			measure_violation_with_rules(
				&final_step(&step(0, |s| s.latencies_ms = vec![11_000]), None),
				&RULES
			)
			.unwrap()
			.0,
			"latency"
		);
		assert_eq!(
			measure_violation_with_rules(&final_step(&step(0, |s| s.rejected = 20), None), &RULES)
				.unwrap()
				.0,
			"refused"
		);
	}

	#[test]
	fn fails_on_pool_refusals_in_the_step_before_as_graceful() {
		let steps = [
			step(0, |s| {
				s.rejected = 200;
				s.errors.insert("1010 Immediately Dropped".into(), 200);
			}),
			step(1, |_| ()),
		];
		let stop = check_step_with_rules(&steps, &source(), 0, 6_000.0, &RULES).unwrap();
		assert_eq!(
			(stop.rule, stop.step, stop.class),
			(Rule::PoolRefuses, Some(0), Some(Class::Graceful))
		);
		assert!(stop.detail.contains("Immediately Dropped"));
	}

	#[test]
	fn fails_on_slow_blocks_as_hard() {
		let blocks = [13_000, 14_000]
			.map(|gap| polkameter_files::BlockRecord { gap_ms: Some(gap), ..Default::default() })
			.to_vec();
		let stop =
			check_step_with_rules(&[step(0, |s| s.blocks = blocks)], &source(), 0, 6_000.0, &RULES)
				.unwrap();
		assert_eq!((stop.rule, stop.class), (Rule::SlowBlocks, Some(Class::Hard)));
	}

	#[test]
	fn ends_without_a_failure_when_the_load_tool_cant_keep_up() {
		let stop = check_step_with_rules(
			&[step(0, |s| {
				s.sent = 500;
				s.included = 500;
			})],
			&source(),
			0,
			6_000.0,
			&RULES,
		)
		.unwrap();
		assert_eq!((stop.rule, stop.class), (Rule::GeneratorLimit, None));
		let stop = check_step_with_rules(
			&[step(0, |s| {
				s.sent = 500;
				s.included = 500;
				s.starved_ticks = 3;
			})],
			&source(),
			0,
			6_000.0,
			&RULES,
		)
		.unwrap();
		assert_eq!(stop.rule, Rule::BudgetUsedUp);
	}
}
