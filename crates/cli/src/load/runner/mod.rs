//! The phases every scenario shares:
//!
//!   1. baseline    a few probe txs, one per block, before any load
//!   2. load        the plan's steps at their rates, until a failure or the end
//!   3. recovery    no load, one probe per block, until probes land in time again
//!   4. watch       while the loss check runs, the chain's stalls and node down still count
//!
//! One loop per phase over three inputs: a tick, submit replies, block events. The tracker owns
//! every tx; this file only decides when to send and when a phase ends. Monitors hear about
//! phases and step edges on a broadcast channel.

use std::time::Duration;

use polkameter_plugin_sdk::observers::{
	FileError, Millis, NodeMax, Problems,
	summary::{Baseline, Class, Outcome, ProbePhase, Recovery, Rule, Stop},
};
use serde::Serialize;
use tokio::{
	sync::{broadcast, mpsc::UnboundedReceiver},
	time::{Instant, MissedTickBehavior},
};

use crate::load::{follower::BlockEvent, now_ms, plan::Plan};

mod load;

use crate::load::{
	recovery::recovered_at,
	rules::{LiveState, Rules, live_failure, secs},
	sender::Reply,
	steps::{block_stats, percentile},
	submit::Submit,
	tracker::Tracker,
};
pub use load::load;
use polkameter_plugin_sdk::observers::Phase;

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
	/// A phase (baseline, recovery) started: scrape now, for the same reason.
	PhaseEdge,
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
	/// A clock that counts sleep ([`crate::load::clock::boot_ms`]), only to see that the
	/// machine was suspended; a test changes it.
	pub boot: fn() -> Option<Millis>,
	/// What a monitor on this machine saw of the node's own process.
	pub node: NodeProcess,
}

/// What a monitor on this machine saw of the node's own process: the only evidence PolkaMeter
/// has that the node, not the network between, failed when the node can't be reached. Empty
/// when no process here is known to be the node.
#[derive(Debug, Clone, Default)]
pub struct NodeProcess(std::sync::Arc<std::sync::Mutex<NodeFacts>>);

#[derive(Debug, Default)]
struct NodeFacts {
	serves_rpc: Option<String>,
	ended: Option<String>,
	stopped: Option<String>,
}

impl NodeProcess {
	fn facts(&self) -> std::sync::MutexGuard<'_, NodeFacts> {
		self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
	}

	/// The node's process serves the RPC endpoint itself, on this machine (`why`): nothing
	/// between PolkaMeter and the node can hold its answers.
	pub fn serves_rpc(&self, why: String) {
		self.facts().serves_rpc = Some(why);
	}

	/// The node's process ended (`why`); the first time counts.
	pub fn ended(&self, why: String) {
		self.facts().ended.get_or_insert(why);
	}

	/// Records a stopped process (`why`). A later running sample (`None`) keeps this evidence.
	pub fn stopped(&self, why: Option<String>) {
		if let Some(why) = why {
			self.facts().stopped.get_or_insert(why);
		}
	}

	/// Why the node itself is to blame for not answering, if its process shows it.
	pub fn why(&self) -> Option<String> {
		let f = self.facts();
		f.ended.clone().or_else(|| f.stopped.clone()).or_else(|| f.serves_rpc.clone())
	}
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

/// A stall of PolkaMeter's own loop longer than this, over the loop's tick period, is a pause of
/// PolkaMeter itself: well above scheduling jitter, and far below any limit of the chain's.
pub const STALL_MS: Millis = 1_000;

/// Why the load tool itself was paused longer than `max_gap` between two inputs, if it was:
/// `elapsed` ms passed on the run's monotonic clock, and `boot` ms on a clock that also counts
/// sleep (`None` without one). A long monotonic gap is CPU starvation; a boot clock far ahead of
/// the monotonic one is a machine that was suspended. The wall clock plays no part: an NTP step
/// either way is ignored.
pub(super) fn paused(elapsed: Millis, boot: Option<Millis>, max_gap: Millis) -> Option<String> {
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

/// One phase's loop: a tick every `period` ms, and the replies and block events between ticks.
/// At every wakeup, before it hands an input to the tracker, it checks how long PolkaMeter went
/// without one. A stall above the period plus [`STALL_MS`] is a pause of PolkaMeter itself (a
/// stopped process, CPU starvation, a suspended machine): the tracker records it, so every time
/// measured across it (latency, reply, block gap, stall clocks) leaves it out, whatever its
/// length. A pause above `max_tick_gap_ms` is also returned by [`Ticks::long_pause`].
pub(super) struct Ticks {
	tick: tokio::time::Interval,
	period: Millis,
	last: Instant,
	boot: Option<Millis>,
	at: Millis,
	long: Option<String>,
}

impl Ticks {
	pub(super) fn new(io: &Io, period: Millis) -> Self {
		let mut tick = tokio::time::interval(Duration::from_millis(period));
		tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
		Self { tick, period, last: Instant::now(), boot: (io.boot)(), at: now_ms(), long: None }
	}

	/// Waits for the next input; true for a tick. Replies and blocks go straight to the tracker.
	pub(super) async fn next<S: Submit>(
		&mut self,
		t: &mut Tracker<S>,
		io: &mut Io,
		rules: &Rules,
	) -> Result<bool, RunError> {
		let e = tokio::select! {
			_ = self.tick.tick() => Event::Tick,
			Some(r) = io.replies.recv() => Event::Reply(r),
			Some(b) = io.blocks.recv() => Event::Block(b),
		};
		self.watch(t, io, rules);
		match e {
			Event::Tick if io.tool_failed.is_cancelled() => return Err(RunError::Monitor),
			Event::Tick => return Ok(true),
			Event::Reply(r) => t.on_reply(r),
			Event::Block(BlockEvent::Tool(e)) => return Err(RunError::Tool(e)),
			Event::Block(b) => t.on_block(b, now_ms())?,
		}
		Ok(false)
	}

	fn watch<S: Submit>(&mut self, t: &mut Tracker<S>, io: &Io, rules: &Rules) {
		let (at, now, boot) = (Instant::now(), now_ms(), (io.boot)());
		let elapsed = at.duration_since(self.last).as_millis() as Millis;
		let slept = boot.zip(self.boot).map(|(b, l)| b.saturating_sub(l));
		if elapsed.max(slept.unwrap_or(0)) > self.period + STALL_MS {
			t.paused(self.at, now);
		}
		if let Some(why) = paused(elapsed, slept, rules.max_tick_gap_ms) {
			self.long.get_or_insert(why);
		}
		(self.last, self.boot, self.at) = (at, boot, now);
	}

	/// Why PolkaMeter was paused longer than `max_tick_gap_ms` since this was last asked.
	pub(super) fn long_pause(&mut self) -> Option<String> {
		self.long.take()
	}
}

/// Why the node is down, if it is: it restarted, or it could not be reached for the reconnect
/// budget and its process here shows that it failed. Both the load and recovery end on it.
pub fn node_down<S: Submit>(t: &Tracker<S>, io: &Io) -> Option<String> {
	let unreachable = || Some(format!("{}; {}", t.unreachable.clone()?, io.node.why()?));
	t.node_down.clone().or_else(unreachable)
}

/// What the live rules read, now.
pub(super) fn live<S: Submit>(t: &Tracker<S>, io: &Io, opts: &RunOptions, step: u32) -> LiveState {
	let now = now_ms();
	LiveState {
		step,
		now,
		last_block_at: t.last_head.0,
		last_finalized_at: t.last_finalized.0,
		best: t.last_head.1,
		finalized: t.last_finalized.1,
		unobserved_since_block: t.unobserved_between(t.last_head.0, now),
		unobserved_since_finalized: t.unobserved_between(t.last_finalized.0, now),
		node_down: node_down(t, io),
		unreachable: t.unreachable.clone(),
		unstable: t.unstable.clone(),
		smoke_problems: if opts.mode == Mode::Smoke { io.problems.all() } else { Vec::new() },
	}
}

/// A chain failure the live rules see now (stall, finality stall, node down), unless it is the
/// `known` stall: one that already held when the phase started, so the load already stopped on
/// it, and the phase measures whether it ends.
fn chain_failure<S: Submit>(
	t: &Tracker<S>,
	io: &Io,
	opts: &RunOptions,
	known: Option<Rule>,
) -> Option<Stop> {
	let state = LiveState { smoke_problems: Vec::new(), ..live(t, io, opts, 0) };
	live_failure(&state, &opts.rules)
		.filter(|stop| stop.class == Some(Class::Hard) && Some(stop.rule) != known)
		.map(|stop| Stop { step: None, ..stop })
}

/// The stall that already holds now, if one does (see [`chain_failure`]).
fn known_stall<S: Submit>(t: &Tracker<S>, io: &Io, opts: &RunOptions) -> Option<Rule> {
	chain_failure(t, io, opts, None)
		.map(|stop| stop.rule)
		.filter(|rule| matches!(rule, Rule::Stall | Rule::FinalityStall))
}

/// 1. Probes, one per block, must all land before any load; else the network is not healthy.
///
/// Probes PolkaMeter could not time (it was paused, or reconnecting) give the baseline no time:
/// `p50_ms` and `max_ms` are then `None`, never 0.
pub async fn baseline<S: Submit>(
	t: &mut Tracker<S>,
	io: &mut Io,
	opts: &RunOptions,
) -> Result<Baseline, RunError> {
	t.set_phase(Phase::Baseline, now_ms())?;
	let _ = io.events.send(RunEvent::PhaseEdge);
	let deadline = Instant::now()
		+ Duration::from_millis(
			(opts.baseline_probes as u64 + 10) * (opts.block_interval_s * 1000.0) as u64,
		);
	let mut ticks = Ticks::new(io, 200);
	let down = |t: &Tracker<S>, io: &Io| {
		node_down(t, io).map(|why| format!("node down: {why}")).or_else(|| {
			t.unreachable.as_ref().map(|why| format!("node or network unreachable: {why}"))
		})
	};
	while Instant::now() < deadline && down(t, io).is_none() {
		let tick = ticks.next(t, io, &opts.rules).await?;
		if let Some(why) = ticks.long_pause() {
			crate::redact::diag!("baseline: {why}");
		}
		if tick {
			t.tick(now_ms())?;
			if t.probes.len() >= opts.baseline_probes
				&& t.probes.iter().all(|p| p.outcome != Outcome::Pending)
			{
				break;
			}
		}
	}
	let bad = t.probes.iter().filter(|p| p.outcome != Outcome::Included).count();
	// What ended the baseline is the cause, and is named, not only the probes it left.
	if let Some(why) = down(t, io) {
		let landed = t.probes.len() - bad;
		return Err(RunError::Baseline(format!(
			"{why}; {landed} of {} probes landed",
			opts.baseline_probes
		)));
	}
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
		max_ms: ms.iter().copied().max(),
	};
	let time =
		|ms: Option<Millis>| ms.map_or("no time".into(), |ms| format!("{} s", secs(ms as f64)));
	crate::redact::diag!(
		"baseline: {} probes landed, p50 {}, max {}",
		b.probes,
		time(b.p50_ms),
		time(b.max_ms)
	);
	Ok(b)
}

/// After the load: reads every block the follower still has queued, until the last head is
/// read or `timeout_ms` passes, so no inclusion is missed by the loss check.
pub async fn drain<S: Submit>(
	t: &mut Tracker<S>,
	io: &mut Io,
	timeout_ms: Millis,
	rules: &Rules,
) -> Result<bool, RunError> {
	let deadline = Instant::now() + Duration::from_millis(timeout_ms);
	let mut ticks = Ticks::new(io, 100);
	while t.last_fetched < t.last_head.1 && Instant::now() < deadline {
		ticks.next(t, io, rules).await?;
		if let Some(why) = ticks.long_pause() {
			crate::redact::diag!("drain: {why}");
		}
	}
	Ok(t.last_fetched >= t.last_head.1)
}

/// 3. No load, one probe per block, until probes land in time again and our backlog drained.
///
/// The chain's own failures end it and are measured: a stall, a finality stall or node down
/// (one the load did not stop on already) is a recovery that is not back, whatever else
/// PolkaMeter saw. Otherwise the recovery is measured only when PolkaMeter observed it: a pause
/// of PolkaMeter, a dropped connection or a probe it could not time before the chain was back
/// leaves it `not measured`, because the chain may have been back meanwhile. So does a chain not
/// back with no `threshold_ms` (the baseline has no time): twice the block interval is the
/// threshold then, which may be too strict to hold against the chain.
#[allow(clippy::too_many_lines)]
pub async fn recover<S: Submit>(
	t: &mut Tracker<S>,
	io: &mut Io,
	opts: &RunOptions,
	threshold_ms: Option<Millis>,
) -> Result<Recovery, RunError> {
	let (threshold_ms, guessed) = match threshold_ms {
		Some(ms) => (ms, false),
		None => ((opts.block_interval_s * 2000.0) as Millis, true),
	};
	t.set_phase(Phase::Recovery, now_ms())?;
	let _ = io.events.send(RunEvent::PhaseEdge);
	let stopped_at = now_ms();
	let backlog_at_stop = t.outstanding().len() as u64;
	crate::redact::diag!(
		"recovery: load stopped with {backlog_at_stop} txs outstanding; one probe per block for up to {} s",
		opts.recovery_s
	);
	let started = Instant::now();
	let deadline = started + Duration::from_secs(u64::from(opts.recovery_s));
	let (mut back_at, mut drained_at, mut last_log) = (None, None, started);
	let mut ticks = Ticks::new(io, 500);
	let known = known_stall(t, io, opts);
	let mut failed = None;
	while Instant::now() < deadline {
		if !ticks.next(t, io, &opts.rules).await? {
			continue;
		}
		if let Some(why) = ticks.long_pause() {
			crate::redact::diag!("recovery: {why}");
		}
		if let Some(stop) = chain_failure(t, io, opts, known) {
			failed = Some(stop);
			break;
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
		if last_log.elapsed() > Duration::from_secs(30) {
			last_log = Instant::now();
			crate::redact::diag!(
				"recovery: {} s, backlog {}",
				started.elapsed().as_secs(),
				t.outstanding().len()
			);
		}
	}
	let ended_at = now_ms();
	let seconds = |at: Option<Millis>| at.map(|at| at.saturating_sub(stopped_at).div_ceil(1000));
	let landed = format!(
		"{} probes in a row landed within {} s",
		opts.rules.probes_in_a_row,
		secs(threshold_ms as f64)
	);
	let unobserved = t.unobserved_why(stopped_at, back_at.unwrap_or(ended_at)).or_else(|| {
		(guessed && back_at.is_none()).then(|| {
			format!(
				"not back within {} s, but the baseline has no time, so that threshold is only twice the block interval",
				secs(threshold_ms as f64)
			)
		})
	});
	let measured = failed.is_some() || unobserved.is_none();
	let detail = match (&failed, &unobserved, back_at) {
		(Some(stop), ..) => format!("{}: {}", stop.rule.name(), stop.detail),
		(None, Some(why), _) => why.clone(),
		(None, None, Some(_)) => landed,
		(None, None, None) if t.recovery_blocks.is_empty() => {
			let s = ended_at.saturating_sub(stopped_at).div_ceil(1000);
			// New heads came, but no block was read: the chain went on, the reads did not.
			if t.last_head.0 > stopped_at {
				format!(
					"no block read in {s} s, though the node announced new blocks up to block {}: reading them was delayed",
					t.last_head.1
				)
			} else {
				format!("no new block in {s} s")
			}
		},
		(None, None, None) => format!("after {} s no {landed}", opts.recovery_s),
	};
	let back_at = back_at.filter(|_| failed.is_none());
	let r = Recovery {
		measured,
		recovered: measured && back_at.is_some(),
		seconds: seconds(back_at.filter(|_| measured)),
		detail,
		threshold_ms,
		backlog_at_stop,
		backlog_at_end: t.outstanding().len() as u64,
		drained_seconds: seconds(drained_at.filter(|_| measured)),
		drain_per_s: measured.then(|| {
			t.drained as f64 / (ended_at.saturating_sub(stopped_at) as f64 / 1000.0).max(0.001)
		}),
		probes: t.probes.iter().filter(|p| p.phase == ProbePhase::Recovery).cloned().collect(),
		blocks: block_stats(&t.recovery_blocks),
		node: NodeMax::default(),
	};
	crate::redact::diag!(
		"recovery: {} ({}); backlog {} left",
		if r.recovered { "back" } else { "not back" },
		r.detail,
		r.backlog_at_end
	);
	Ok(r)
}

/// 4. While `work` runs (the loss check), follows the chain on.
///
/// A stall, a finality stall or node down that starts or goes on meanwhile is the chain's
/// failure, as in the load. One that already held when it started was reported already.
/// Returns what `work` gave, and the failure.
pub async fn watch<S: Submit, T>(
	t: &mut Tracker<S>,
	io: &mut Io,
	opts: &RunOptions,
	work: impl std::future::Future<Output = T>,
) -> Result<(T, Option<Stop>), RunError> {
	let mut ticks = Ticks::new(io, 500);
	// What already held was reported by the load or recovery.
	let known = chain_failure(t, io, opts, None).map(|stop| stop.rule);
	let mut failed = None;
	tokio::pin!(work);
	loop {
		tokio::select! {
			out = &mut work => return Ok((out, failed)),
			tick = ticks.next(t, io, &opts.rules), if failed.is_none() => {
				if tick? {
					failed = chain_failure(t, io, opts, known);
				}
			},
		}
	}
}
