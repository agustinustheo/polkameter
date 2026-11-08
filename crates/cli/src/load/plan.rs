//! How fast to send, step by step and lane by lane. Each step is one window for the stop rules
//! and the checks; the first failure ends the load.

use crate::load::source::QueueSource;

/// One load of a run: its own txs, rate and counts. The `call` names it in the load series.
pub struct Lane {
	/// `call` label, e.g. `Resources.set_statement_store_account`.
	pub call: &'static str,
	/// Its txs.
	pub source: QueueSource,
}

/// One step: how long, and the target rate of each lane.
#[derive(Debug, Clone, PartialEq)]
pub struct StepPlan {
	/// Length.
	pub seconds: u32,
	/// Target tx/s per lane, in lane order.
	pub rates: Vec<f64>,
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
		Self::one_lane(interval_s, steps, |k| start + f64::from(k) * step)
	}

	/// One lane: `start` tx/s, times `growth` every `interval_s`, for `steps` steps.
	pub fn geometric(start: f64, growth: f64, interval_s: u32, steps: u32) -> Self {
		Self::one_lane(interval_s, steps, |k| start * growth.powf(f64::from(k)))
	}

	/// `steps` steps of `interval_s` seconds, one lane at `rate(k)` tx/s in step `k`.
	fn one_lane(interval_s: u32, steps: u32, rate: impl Fn(u32) -> f64) -> Self {
		let steps = (0..steps)
			.map(|k| StepPlan { seconds: interval_s, rates: vec![rate(k)] })
			.collect();
		Self { steps }
	}

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
				let rates: Vec<String> = s.rates.iter().map(f64::to_string).collect();
				return Err(format!("step {k}: {} s at {} tx/s", s.seconds, rates.join(", ")));
			}
		}
		Ok(())
	}
}
