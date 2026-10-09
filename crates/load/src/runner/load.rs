//! 2. The load: the plan's steps, every lane at its own token bucket, and when a stop ends it.

use polkameter_files::summary::{Rule, Stop};

use super::{Io, Mode, RunError, RunEvent, RunOptions, interval, next};
use crate::{
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
		t.start_step(k, &plan.rates, now_ms())?;
		let _ = io.events.send(RunEvent::StepEdge(k));
		let ended = send_step(t, io, opts, &plan.rates, plan.seconds).await?;
		t.end_step(now_ms());
		let _ = io.events.send(RunEvent::StepEdge(k));
		log_step(t);
		if !breaking
			&& let Some((_, detail)) = t.steps[0]
				.len()
				.checked_sub(2)
				.and_then(|i| measure_violation(&final_step(&t.steps[0][i], None), &opts.rules))
		{
			breaking = true;
			eprintln!("breaking point: step {}: {detail}; the load goes on until a failure", k - 1);
		}
		if let Some(stop) = ended {
			return Ok(stop);
		}
		for (lane, steps) in t.steps.iter().enumerate() {
			let source = &t.lanes()[lane].source;
			let unsent_kib = t.sender().queued_bytes() / 1024;
			if let Some(stop) = check_step(steps, source, unsent_kib, block_ms, &opts.rules) {
				return Ok(stop);
			}
		}
	}
	let last = opts.plan.steps.len().saturating_sub(1) as u32;
	Ok(Stop::new(Rule::RateCap, Some(last), format!("all {} steps ran", opts.plan.steps.len())))
}

/// Sends at the step's rates until it ends; a failure during the step ends it and the load.
async fn send_step<S: Submit>(
	t: &mut Tracker<S>,
	io: &mut Io,
	opts: &RunOptions,
	rates: &[f64],
	seconds: u32,
) -> Result<Option<Stop>, RunError> {
	let end = now_ms() + u64::from(seconds) * 1000;
	let mut tick = interval(20);
	let mut tokens = vec![0.0_f64; rates.len()];
	let mut last = now_ms();
	let step = t.steps[0].last().expect("started").step;
	while now_ms() < end {
		if !next(t, io, &mut tick).await? {
			continue;
		}
		let now = now_ms();
		if now - last > opts.rules.max_tick_gap_ms {
			let detail = format!(
				"the load tool was paused for {} s (machine sleep or CPU starvation); the numbers after this are not the chain's",
				(now - last) / 1000
			);
			return Ok(Some(Stop::new(Rule::GeneratorLimit, Some(step), detail)));
		}
		for (lane, rate) in rates.iter().enumerate() {
			tokens[lane] = (tokens[lane] + rate * (now - last) as f64 / 1000.0).min(rate.max(1.0)); // at most 1 s of catch-up
			while tokens[lane] >= 1.0 && t.send_next(lane, now) {
				tokens[lane] -= 1.0;
			}
			if tokens[lane] >= 1.0 {
				tokens[lane] = 0.0; // nothing ready, or backed up
			}
		}
		last = now;
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
			smoke_problems: if opts.mode == Mode::Smoke { io.problems.all() } else { Vec::new() },
		};
		if let Some(stop) = live_failure(&live, &opts.rules) {
			return Ok(Some(stop));
		}
	}
	Ok(None)
}

fn log_step<S: Submit>(t: &Tracker<S>) {
	for (lane, steps) in t.steps.iter().enumerate() {
		let f = final_step(steps.last().expect("started"), None);
		eprintln!(
			"step {} {}: target {} tx/s, sent {:.0} tx/s, {} blocks, {:.1} tx/s included, reply p95 {} s, {} replies pending, backlog {}",
			f.step,
			t.lanes()[lane].call,
			f.target_rate,
			f.sent_per_s,
			f.blocks.blocks,
			f.included_per_s,
			secs(f.p95_reply_ms as f64),
			t.inflight(),
			t.outstanding().len()
		);
	}
}
