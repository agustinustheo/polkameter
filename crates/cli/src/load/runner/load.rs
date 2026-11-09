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
		if let Some(why) = node_down(t) {
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
	// Monotonic time for the step's end and the token bucket: a wall-clock step neither ends the
	// step early nor sends a burst.
	let start = Instant::now();
	let end = start + Duration::from_secs(u64::from(seconds));
	let mut ticks = Ticks::new(io, 20);
	let mut tokens = 0.0_f64;
	let mut last = start;
	let step = t.steps.last().expect("started").step;
	while Instant::now() < end {
		let tick = ticks.next(t, io, &opts.rules).await?;
		if let Some(detail) = ticks.long_pause() {
			return Ok(Some(Stop::new(Rule::GeneratorLimit, Some(step), detail)));
		}
		if !tick {
			continue;
		}
		let (at, now) = (Instant::now(), now_ms());
		let elapsed = at.duration_since(last);
		tokens = (tokens + rate * elapsed.as_secs_f64()).min(rate.max(1.0)); // at most 1 s of catch-up
		while tokens >= 1.0 && t.send_next(now) {
			tokens -= 1.0;
		}
		if tokens >= 1.0 {
			tokens = 0.0; // nothing ready, or backed up
		}
		last = at;
		t.tick(now)?;
		if let Some(stop) = live_failure(&live(t, io, opts, step), &opts.rules) {
			return Ok(Some(stop));
		}
	}
	Ok(None)
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
