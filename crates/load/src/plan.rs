//! How fast to send, step by step and lane by lane. Each step is one window for the stop rules
//! and the checks; the first failure ends the load.

use crate::source::LoadSource;

/// One load of a run: its own txs, rate and counts. The `call` names it in the load series.
pub struct Lane {
	/// `call` label, e.g. `Resources.set_statement_store_account`.
	pub call: &'static str,
	/// Its txs.
	pub source: Box<dyn LoadSource>,
}

impl std::fmt::Debug for Lane {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Lane").field("call", &self.call).finish_non_exhaustive()
	}
}

/// One step: how long, and the target rate of each lane.
#[derive(Debug, Clone, PartialEq)]
pub struct StepPlan {
	/// Length.
	pub seconds: u32,
	/// Target tx/s per lane, in lane order.
	pub rates: Vec<f64>,
}

/// A scenario's ramp defaults; every field can be changed from the command line.
#[derive(Debug, Clone, Copy, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Ramp {
	/// First step's rate, tx/s.
	pub start: f64,
	/// Rate added per step.
	pub step: f64,
	/// Rate multiplier per step; replaces `step` when set.
	pub growth: Option<f64>,
	/// Step length, s.
	pub interval_s: u32,
	/// Steps.
	pub steps: u32,
	/// Recovery budget, s.
	pub recovery_s: u32,
	/// Baseline probes.
	pub probes: usize,
}

/// The steps of a run.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
	/// The steps.
	pub steps: Vec<StepPlan>,
}

impl Plan {
	/// One lane: `start` tx/s, plus `step` every `interval_s`, for `steps` steps.
	pub fn ramp(start: f64, step: f64, interval_s: u32, steps: u32) -> Self {
		let steps = (0..steps)
			.map(|k| StepPlan { seconds: interval_s, rates: vec![start + f64::from(k) * step] })
			.collect();
		Self { steps }
	}

	/// One lane: `start` tx/s, times `growth` every `interval_s`, for `steps` steps.
	pub fn geometric(start: f64, growth: f64, interval_s: u32, steps: u32) -> Self {
		let steps = (0..steps)
			.map(|k| StepPlan {
				seconds: interval_s,
				rates: vec![start * growth.powf(f64::from(k))],
			})
			.collect();
		Self { steps }
	}
}

impl Plan {
	/// Checks the plan before any setup: steps, one rate per lane in every step, no negative
	/// rate. `lanes` is checked once the scenario built them.
	pub fn check(&self, lanes: Option<usize>) -> Result<(), String> {
		let first = self.steps.first().ok_or("the plan has no step")?;
		let n = lanes.unwrap_or(first.rates.len());
		for (k, s) in self.steps.iter().enumerate() {
			if s.rates.len() != n {
				return Err(format!("step {k} has {} rates for {n} lanes", s.rates.len()));
			}
			if s.seconds == 0 || s.rates.iter().any(|r| !r.is_finite() || *r < 0.0) {
				return Err(format!("step {k}: {} s at {:?} tx/s", s.seconds, s.rates));
			}
		}
		Ok(())
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn a_plan_is_checked_against_its_lanes() {
		assert!(Plan::ramp(6.0, 4.0, 60, 10).check(Some(1)).is_ok());
		assert!(
			Plan::ramp(6.0, 4.0, 60, 10)
				.check(Some(2))
				.unwrap_err()
				.contains("1 rates for 2 lanes")
		);
		let uneven = Plan {
			steps: vec![
				StepPlan { seconds: 60, rates: vec![1.0, 5.0] },
				StepPlan { seconds: 60, rates: vec![2.0] },
			],
		};
		assert!(uneven.check(None).is_err());
		assert!(Plan::ramp(6.0, 4.0, 60, 0).check(None).is_err());
	}

	#[test]
	fn a_geometric_plan_multiplies_the_rate() {
		let rates: Vec<f64> =
			Plan::geometric(10.0, 2.0, 60, 5).steps.iter().map(|s| s.rates[0]).collect();
		assert_eq!(rates, [10.0, 20.0, 40.0, 80.0, 160.0]);
	}
}
