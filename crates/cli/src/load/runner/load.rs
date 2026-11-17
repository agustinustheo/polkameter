//! 2. The load: the plan's steps through a token bucket, and when a stop ends it.

use std::time::Duration;

use polkameter_plugin_sdk::observers::summary::{Rule, Stop};
use tokio::time::Instant;

use super::{Io, RunError, RunEvent, RunOptions, Ticks, live, node_down};
use crate::load::{
	now_ms,
	rules::{check_step, live_failure, measure_violation, rate, secs},
	steps::final_step,
	submit::Submit,
	tracker::Tracker,
};
use polkameter_plugin_sdk::observers::Phase;

/// 2. The plan's steps, until the first failure or the last step. Returns why the load ended.
pub async fn load<S: Submit>(
	t: &mut Tracker<S>,
	io: &mut Io,
	opts: &RunOptions,
) -> Result<Stop, RunError> {
	t.set_phase(Phase::Ramp, now_ms())?;
	let block_ms = opts.block_interval_s * 1000.0;
	let mut breaking = false;
	for (k, plan) in opts.plan.steps.iter().enumerate() {
		let k = k as u32;
		t.start_step(k, plan.rate, now_ms())?;
		let _ = io.events.send(RunEvent::StepEdge(k));
		let ended = send_step(t, io, opts, plan.rate, plan.seconds).await?;
		t.end_step(now_ms());
		let _ = io.events.send(RunEvent::StepEdge(k));
		log_step(t);
		if !breaking
			&& let Some((_, detail)) = t
				.steps
				.len()
				.checked_sub(2)
				.and_then(|i| measure_violation(&final_step(&t.steps[i]), &opts.rules))
		{
			breaking = true;
			crate::redact::diag!(
				"breaking point: step {}: {detail}; the load goes on until a failure",
				k - 1
			);
		}
		if let Some(stop) = ended {
			return Ok(stop);
		}
		// The follower may have seen the node restart only as the step ended: that, not the
		// step's low send rate, is why it sent too little.
		if let Some(why) = node_down(t, io) {
			return Ok(Stop::new(Rule::NodeDown, Some(k), why));
		}
		// After the last step an empty flood only means it was sized to the plan.
		let last = k as usize + 1 == opts.plan.steps.len();
		let unsent_kib = t.sender().queued_bytes() / 1024;
		if let Some(stop) =
			check_step(&t.steps, t.source(), unsent_kib, block_ms, &opts.rules, last)
		{
			return Ok(stop);
		}
	}
	let last = opts.plan.steps.len().saturating_sub(1) as u32;
	Ok(Stop::new(Rule::RateCap, Some(last), format!("all {} steps ran", opts.plan.steps.len())))
}

/// Sends at the step's rate until it ends; a failure during the step ends it and the load.
async fn send_step<S: Submit>(
	t: &mut Tracker<S>,
	io: &mut Io,
	opts: &RunOptions,
	rate: f64,
	seconds: u32,
) -> Result<Option<Stop>, RunError> {
	// Monotonic time for the step's end and its schedule: a wall-clock step neither ends the
	// step early nor sends a burst. Transaction k of the step is due at start + k / rate, so
	// exactly rate × seconds are sent when the generator keeps up, whatever the tick timing.
	let start = Instant::now();
	let end = start + Duration::from_secs(u64::from(seconds));
	let mut schedule = Schedule::new(rate, seconds);
	let mut ticks = Ticks::new(io, 20);
	let step = t.steps.last().expect("started").step;
	while Instant::now() < end {
		let tick = ticks.next(t, io, &opts.rules).await?;
		if let Some(detail) = ticks.long_pause() {
			return Ok(Some(Stop::new(Rule::GeneratorLimit, Some(step), detail)));
		}
		if !tick {
			continue;
		}
		let now = now_ms();
		schedule.send(Instant::now().duration_since(start), |_| t.send_next(now));
		t.tick(now)?;
		if let Some(stop) = live_failure(&live(t, io, opts, step), &opts.rules) {
			return Ok(Some(stop));
		}
	}
	// The last ones due, when the step's last tick came just before its end.
	let now = now_ms();
	schedule.send(end.duration_since(start), |_| t.send_next(now));
	Ok(None)
}

/// A step's send schedule on its own clock: transaction `k` is due at `k / rate` seconds, for
/// `rate × seconds` transactions. What could not be sent (nothing ready, or the sender backed
/// up) is not sent later, and neither is more than 1 s of a schedule fallen behind: the load is
/// then short, and the step's send ratio shows it.
pub struct Schedule {
	rate: f64,
	total: u64,
	/// Transactions sent or given up so far.
	done: u64,
}

impl Schedule {
	/// The schedule of `rate` tx/s for `seconds`.
	pub fn new(rate: f64, seconds: u32) -> Self {
		let total = (rate * f64::from(seconds) + 1e-9).floor().max(0.0) as u64;
		Self { rate, total, done: 0 }
	}

	/// Sends what is due at `elapsed` through `send`, which says whether it sent one.
	pub fn send(&mut self, elapsed: Duration, mut send: impl FnMut(u64) -> bool) {
		let due = ((elapsed.as_secs_f64() * self.rate + 1e-9).floor() as u64 + 1).min(self.total);
		let late = self.rate.max(1.0).floor() as u64;
		self.done = self.done.max(due.saturating_sub(late));
		while self.done < due && send(self.done) {
			self.done += 1;
		}
		self.done = self.done.max(due);
	}
}

fn log_step<S: Submit>(t: &Tracker<S>) {
	let f = final_step(t.steps.last().expect("started"));
	crate::redact::diag!(
		"step {} prepared: target {} tx/s, sent {} tx/s, {} blocks, {:.1} tx/s included, reply p95 {}, {} replies pending, backlog {}",
		f.step,
		f.target_rate,
		rate(f.sent_per_s),
		f.blocks.blocks,
		f.included_per_s,
		f.p95_reply_ms.map_or("no time".into(), |ms| format!("{} s", secs(ms as f64))),
		t.inflight(),
		t.outstanding().len()
	);
}

#[cfg(test)]
mod tests {
	use super::*;

	/// 1 tx/s for 120 s with ticks every 20 ms that drift (each a little late): exactly 120,
	/// all within the step.
	#[test]
	fn the_schedule_sends_rate_times_seconds() {
		for (rate, seconds) in [(1.0, 120), (5.0, 120), (0.5, 120), (3.0, 20), (120.0, 10)] {
			let mut schedule = Schedule::new(rate, seconds);
			let mut sent = Vec::new();
			let mut ms = 0u64;
			while ms < u64::from(seconds) * 1000 {
				ms += 20 + ms % 7; // late ticks
				let at = Duration::from_millis(ms.min(u64::from(seconds) * 1000));
				schedule.send(at, |k| {
					sent.push((k, at));
					true
				});
			}
			let want = (rate * f64::from(seconds)) as u64;
			assert_eq!(sent.len() as u64, want, "{rate} tx/s for {seconds} s");
			assert!(sent.iter().all(|(_, at)| *at <= Duration::from_secs(u64::from(seconds))));
		}
	}

	/// What could not be sent is not sent later, and a schedule more than 1 s behind skips
	/// ahead: no catch-up burst.
	#[test]
	fn the_schedule_never_catches_up_more_than_a_second() {
		let mut schedule = Schedule::new(10.0, 60);
		let mut sent = 0;
		schedule.send(Duration::from_secs(1), |_| {
			sent += 1;
			true
		});
		assert_eq!(sent, 10, "1 s of a 10 tx/s schedule after the first tick at 1 s");
		schedule.send(Duration::from_secs(2), |_| false);
		let mut later = 0;
		schedule.send(Duration::from_secs(12), |_| {
			later += 1;
			true
		});
		assert_eq!(later, 10, "10 s behind: only the last second is sent");
	}
}
