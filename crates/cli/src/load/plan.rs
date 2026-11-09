//! How fast to send, step by step. Each step is one window for the stop rules
//! and the checks; the first failure ends the load.

/// One step: how long, and the target rate.
#[derive(Debug, Clone, PartialEq)]
pub struct StepPlan {
	/// Length.
	pub seconds: u32,
	/// Target tx/s.
	pub rate: f64,
}

/// The steps of a run.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
	/// The steps.
	pub steps: Vec<StepPlan>,
}

impl Plan {
	/// `start` tx/s, plus `step` every `interval_s`, for `steps` steps.
	pub fn ramp(start: f64, step: f64, interval_s: u32, steps: u32) -> Self {
		Self::stepped(interval_s, steps, |k| start + f64::from(k) * step)
	}

	/// `start` tx/s, times `growth` every `interval_s`, for `steps` steps.
	pub fn geometric(start: f64, growth: f64, interval_s: u32, steps: u32) -> Self {
		Self::stepped(interval_s, steps, |k| start * growth.powf(f64::from(k)))
	}

	/// `steps` steps of `interval_s` seconds, at `rate(k)` tx/s in step `k`.
	fn stepped(interval_s: u32, steps: u32, rate: impl Fn(u32) -> f64) -> Self {
		let steps = (0..steps).map(|k| StepPlan { seconds: interval_s, rate: rate(k) }).collect();
		Self { steps }
	}

	/// Checks the plan before any setup: at least one step, and no step without time or with a
	/// negative rate.
	pub fn check(&self) -> Result<(), String> {
		if self.steps.is_empty() {
			return Err("the plan has no step".into());
		}
		for (k, s) in self.steps.iter().enumerate() {
			if s.seconds == 0 || !s.rate.is_finite() || s.rate < 0.0 {
				return Err(format!("step {k}: {} s at {} tx/s", s.seconds, s.rate));
			}
		}
		Ok(())
	}
}
