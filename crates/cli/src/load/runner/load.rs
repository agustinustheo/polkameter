//! 2. The load: the plan's steps through a token bucket, and when a stop ends it.

use std::time::Duration;

use polkameter_plugin_sdk::observers::{
	Millis,
	summary::{Rule, Stop},
};
use tokio::time::Instant;

use super::{Io, Mode, RunError, RunEvent, RunOptions, interval, next, node_down};
use crate::load::{
	now_ms,
	rules::{LiveState, check_step, live_failure, measure_violation, secs},
	steps::final_step,
	submit::Submit,
	tracker::{Phase, Tracker},
};

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
		// After the last step an empty flood only means it was sized to the plan.
		let last = k as usize + 1 == opts.plan.steps.len();
		let unsent_kib = t.sender().queued_bytes() / 1024;
		let source = &t.lane().source;
		if let Some(stop) = check_step(&t.steps, source, unsent_kib, block_ms, &opts.rules, last) {
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
	let mut tick = interval(20);
	let mut tokens = 0.0_f64;
	let (mut last, mut last_boot) = (start, (io.boot)());
	let step = t.steps.last().expect("started").step;
	while Instant::now() < end {
		if !next(t, io, &mut tick).await? {
			continue;
		}
		let (at, now, boot) = (Instant::now(), now_ms(), (io.boot)());
		let elapsed = at.duration_since(last);
		let elapsed_ms = elapsed.as_millis() as Millis;
		if let Some(detail) = paused(
			elapsed_ms,
			boot.zip(last_boot).map(|(b, l)| b.saturating_sub(l)),
			opts.rules.max_tick_gap_ms,
		) {
			return Ok(Some(Stop::new(Rule::GeneratorLimit, Some(step), detail)));
		}
		tokens = (tokens + rate * elapsed.as_secs_f64()).min(rate.max(1.0)); // at most 1 s of catch-up
		while tokens >= 1.0 && t.send_next(now) {
			tokens -= 1.0;
		}
		if tokens >= 1.0 {
			tokens = 0.0; // nothing ready, or backed up
		}
		(last, last_boot) = (at, boot);
		t.tick(now)?;
		let live = LiveState {
			step,
			now,
			closed_by_node: t.closed_by_node,
			connections: opts.connections,
			last_block_at: t.last_head.0,
			last_finalized_at: t.last_finalized.0,
			best: t.last_head.1,
			finalized: t.last_finalized.1,
			unobserved_since_block: t.unobserved_between(t.last_head.0, now),
			unobserved_since_finalized: t.unobserved_between(t.last_finalized.0, now),
			node_down: node_down(t, io),
			smoke_problems: if opts.mode == Mode::Smoke { io.problems.all() } else { Vec::new() },
		};
		if let Some(stop) = live_failure(&live, &opts.rules) {
			return Ok(Some(stop));
		}
	}
	Ok(None)
}

/// Why the load tool itself was paused between two send ticks, if it was: `elapsed` ms passed on
/// the run's monotonic clock, and `boot` ms on a clock that also counts sleep (`None` without
/// one). A long monotonic gap is CPU starvation; a boot clock far ahead of the monotonic one is a
/// machine that was suspended. The wall clock plays no part: an NTP step either way is ignored.
fn paused(elapsed: Millis, boot: Option<Millis>, max_gap: Millis) -> Option<String> {
	const NOT_THE_CHAIN: &str = "the numbers after this are not the chain's";
	if elapsed > max_gap {
		return Some(format!(
			"the load tool was paused for {} s (CPU starvation or a suspended machine); {NOT_THE_CHAIN}",
			elapsed / 1000
		));
	}
	let slept = boot?.saturating_sub(elapsed);
	(slept > max_gap)
		.then(|| format!("the machine was suspended for {} s; {NOT_THE_CHAIN}", slept / 1000))
}

fn log_step<S: Submit>(t: &Tracker<S>) {
	let f = final_step(t.steps.last().expect("started"));
	crate::redact::diag!(
		"step {} {}: target {} tx/s, sent {:.0} tx/s, {} blocks, {:.1} tx/s included, reply p95 {} s, {} replies pending, backlog {}",
		f.step,
		t.lane().call,
		f.target_rate,
		f.sent_per_s,
		f.blocks.blocks,
		f.included_per_s,
		secs(f.p95_reply_ms as f64),
		t.inflight(),
		t.outstanding().len()
	);
}
