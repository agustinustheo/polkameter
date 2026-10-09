//! The phases every scenario shares:
//!
//!   1. baseline    a few probe txs, one per block, before any load
//!   2. load        the plan's steps, every lane at its own rate, until a failure or the end
//!   3. recovery    no load, one probe per block, until probes land in time again
//!
//! One loop per phase over three inputs: a tick, submit replies, block events. The tracker owns
//! every tx; this file only decides when to send and when a phase ends. Monitors hear about
//! phases and step edges on a broadcast channel, so they can live in another process later.

use std::time::Duration;

use polkameter_files::{
	FileError, Millis, NodeMax, Problems,
	summary::{Baseline, Outcome, ProbePhase, Recovery},
};
use serde::Serialize;
use tokio::{
	sync::{broadcast, mpsc::UnboundedReceiver},
	time::MissedTickBehavior,
};

use crate::{follower::BlockEvent, now_ms, plan::Plan};

mod load;

use crate::{
	recovery::recovered_at,
	rules::{Rules, secs},
	sender::Reply,
	steps::{block_stats, percentile},
	submit::Submit,
	tracker::{Phase, Tracker},
};
pub use load::load;

/// smoke: stop at the first problem of any part; stress: only errors of our own tools stop a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Mode {
	/// Short and strict.
	Smoke,
	/// The measurement.
	Stress,
}

impl std::fmt::Display for Mode {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.write_str(match self {
			Self::Smoke => "smoke",
			Self::Stress => "stress",
		})
	}
}

impl std::str::FromStr for Mode {
	type Err = String;

	fn from_str(s: &str) -> Result<Self, Self::Err> {
		match s {
			"smoke" => Ok(Self::Smoke),
			"stress" => Ok(Self::Stress),
			other => Err(format!("must be smoke or stress, not {other}")),
		}
	}
}

/// What the monitors hear from the runner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunEvent {
	/// A step started or ended: scrape now, so the window starts and ends on fresh samples.
	StepEdge(u32),
}

/// How to run.
#[derive(Debug, Clone)]
pub struct RunOptions {
	/// Thresholds recorded with this run.
	pub rules: Rules,
	/// smoke or stress.
	pub mode: Mode,
	/// The steps.
	pub plan: Plan,
	/// Sender connections.
	pub connections: usize,
	/// How long to wait for recovery.
	pub recovery_s: u32,
	/// Probes before the load.
	pub baseline_probes: usize,
	/// The chain's block interval before the run.
	pub block_interval_s: f64,
}

/// The loop's inputs and outputs besides the tracker.
#[derive(Debug)]
pub struct Io {
	/// Submit replies.
	pub replies: UnboundedReceiver<Reply>,
	/// Block events.
	pub blocks: UnboundedReceiver<BlockEvent>,
	/// To the monitors.
	pub events: broadcast::Sender<RunEvent>,
	/// What the monitors could not record.
	pub problems: Problems,
	/// Cancelled when a monitor fails with an error of our tools: the run stops at once.
	pub tool_failed: tokio_util::sync::CancellationToken,
}

/// No result: the network was not healthy before the load, or a raw file could not be written.
#[derive(Debug, thiserror::Error)]
pub enum RunError {
	/// Baseline probes did not land.
	#[error("baseline: {0}")]
	Baseline(String),
	/// A raw file could not be written.
	#[error(transparent)]
	File(#[from] FileError),
	/// A monitor failed with an error of our tools.
	#[error("a monitor failed; see its error")]
	Monitor,
	/// The follower read an answer that does not decode as our types.
	#[error("blocks: {0}")]
	Tool(String),
}

enum Event {
	Tick,
	Reply(Reply),
	Block(BlockEvent),
}

pub(super) fn interval(ms: u64) -> tokio::time::Interval {
	let mut i = tokio::time::interval(Duration::from_millis(ms));
	i.set_missed_tick_behavior(MissedTickBehavior::Skip);
	i
}

/// Waits for the next input; true for a tick. Replies and blocks go straight to the tracker.
pub(super) async fn next<S: Submit>(
	t: &mut Tracker<S>,
	io: &mut Io,
	tick: &mut tokio::time::Interval,
) -> Result<bool, RunError> {
	let e = tokio::select! {
		_ = tick.tick() => Event::Tick,
		Some(r) = io.replies.recv() => Event::Reply(r),
		Some(b) = io.blocks.recv() => Event::Block(b),
	};
	match e {
		Event::Tick if io.tool_failed.is_cancelled() => return Err(RunError::Monitor),
		Event::Tick => return Ok(true),
		Event::Reply(r) => t.on_reply(r),
		Event::Block(BlockEvent::Tool(e)) => return Err(RunError::Tool(e)),
		Event::Block(b) => t.on_block(b, now_ms())?,
	}
	Ok(false)
}

/// 1. Probes, one per block, must all land before any load; else the network is not healthy.
pub async fn baseline<S: Submit>(
	t: &mut Tracker<S>,
	io: &mut Io,
	opts: &RunOptions,
) -> Result<Baseline, RunError> {
	t.set_phase(Phase::Baseline, now_ms())?;
	let deadline =
		now_ms() + (opts.baseline_probes as u64 + 10) * (opts.block_interval_s * 1000.0) as u64;
	let mut tick = interval(200);
	while now_ms() < deadline {
		if next(t, io, &mut tick).await? {
			t.tick(now_ms())?;
			if t.probes.len() >= opts.baseline_probes
				&& t.probes.iter().all(|p| p.outcome != Outcome::Pending)
			{
				break;
			}
		}
	}
	let bad = t.probes.iter().filter(|p| p.outcome != Outcome::Included).count();
	if t.probes.len() < opts.baseline_probes || bad > 0 {
		return Err(RunError::Baseline(format!(
			"{} of {} probes landed; the network is not healthy before the load",
			t.probes.len() - bad,
			opts.baseline_probes
		)));
	}
	let ms: Vec<Millis> = t.probes.iter().filter_map(|p| p.latency_ms).collect();
	let b = Baseline {
		probes: t.probes.len(),
		p50_ms: percentile(&ms, 50.0),
		max_ms: ms.iter().copied().max().unwrap_or(0),
	};
	eprintln!(
		"baseline: {} probes landed, p50 {} s, max {} s",
		b.probes,
		secs(b.p50_ms as f64),
		secs(b.max_ms as f64)
	);
	Ok(b)
}

/// After the load: reads every block the follower still has queued, until the last head is
/// read or `timeout_ms` passes, so no inclusion is missed by the loss check.
pub async fn drain<S: Submit>(
	t: &mut Tracker<S>,
	io: &mut Io,
	timeout_ms: Millis,
) -> Result<bool, RunError> {
	let deadline = now_ms() + timeout_ms;
	let mut tick = interval(100);
	while t.last_fetched < t.last_head.1 && now_ms() < deadline {
		next(t, io, &mut tick).await?;
	}
	Ok(t.last_fetched >= t.last_head.1)
}

/// 3. No load, one probe per block, until probes land in time again and our backlog drained.
pub async fn recover<S: Submit>(
	t: &mut Tracker<S>,
	io: &mut Io,
	opts: &RunOptions,
	threshold_ms: Millis,
) -> Result<Recovery, RunError> {
	t.set_phase(Phase::Recovery, now_ms())?;
	let stopped_at = now_ms();
	let backlog_at_stop = t.outstanding().len() as u64;
	eprintln!(
		"recovery: load stopped with {backlog_at_stop} txs outstanding; one probe per block for up to {} s",
		opts.recovery_s
	);
	let deadline = stopped_at + u64::from(opts.recovery_s) * 1000;
	let (mut back_at, mut drained_at, mut last_log) = (None, None, stopped_at);
	let mut tick = interval(500);
	while now_ms() < deadline && t.closed_by_node == 0 {
		if !next(t, io, &mut tick).await? {
			continue;
		}
		let now = now_ms();
		t.tick(now)?;
		back_at = back_at.or_else(|| {
			recovered_at(
				&t.probes,
				&t.recovery_blocks,
				threshold_ms,
				opts.block_interval_s * 1000.0,
				&opts.rules,
			)
		});
		if t.outstanding().is_empty() {
			drained_at = drained_at.or(Some(now));
		}
		if back_at.is_some() && drained_at.is_some() {
			break;
		}
		if now - last_log > 30_000 {
			last_log = now;
			eprintln!(
				"recovery: {} s, backlog {}",
				(now - stopped_at) / 1000,
				t.outstanding().len()
			);
		}
	}
	let seconds = |at: Option<Millis>| at.map(|at| at.saturating_sub(stopped_at).div_ceil(1000));
	let landed = format!(
		"{} probes in a row landed within {} s",
		opts.rules.probes_in_a_row,
		secs(threshold_ms as f64)
	);
	let detail = match back_at {
		Some(_) => landed,
		None if t.recovery_blocks.is_empty() => format!("no new block in {} s", opts.recovery_s),
		None => format!("after {} s no {landed}", opts.recovery_s),
	};
	let r = Recovery {
		measured: true,
		recovered: back_at.is_some(),
		seconds: seconds(back_at),
		detail,
		threshold_ms,
		backlog_at_stop,
		backlog_at_end: t.outstanding().len() as u64,
		drained_seconds: seconds(drained_at),
		drain_per_s: t.drained as f64
			/ (now_ms().saturating_sub(stopped_at) as f64 / 1000.0).max(0.001),
		probes: t.probes.iter().filter(|p| p.phase == ProbePhase::Recovery).cloned().collect(),
		blocks: block_stats(&t.recovery_blocks),
		node: NodeMax::default(),
	};
	eprintln!(
		"recovery: {} ({}); backlog {} left",
		if r.recovered { "back" } else { "not back" },
		r.detail,
		r.backlog_at_end
	);
	Ok(r)
}
