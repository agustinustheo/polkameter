//! Every tx we sent, and what became of it. Plain code with no clock and no IO of its own
//! besides the two files it owns (`load.jsonl`, `blocks.jsonl`): the runner feeds it the time,
//! the send tick, the submit replies and the block events. Flood txs count towards the step they
//! were sent in; probes are apart. Block events are in [`blocks`].

mod blocks;

use std::collections::HashMap;

use polkameter_plugin_sdk::observers::{
	BlockRecord, FileError, JsonlWriter, Millis, Phase, SeriesWriter,
	registry::{PHASE, STEP, TX_REJECTED, TX_SENT},
	summary::{Outcome, Probe},
};

use crate::load::{
	sender::Reply,
	source::{QueueSource, TxHash},
	steps::StepStats,
	submit::Submit,
};

/// The `call` label of the load series: every tx is a prepared one.
const CALL: &str = "prepared";

/// What a tx was sent as.
#[derive(Debug, Clone, Copy)]
enum Kind {
	/// A flood tx, counted in step `step`.
	Flood { step: usize },
	/// A baseline or recovery probe: `probes[index]`.
	Probe { index: usize },
}

#[derive(Debug)]
struct Sent {
	kind: Kind,
	sent_at: Millis,
}

/// A flood tx as sent, kept to the end for the loss check.
#[derive(Debug, Clone, Default)]
pub struct FloodTx {
	/// The step that sent it.
	pub step: usize,
	/// When.
	pub sent_at: Millis,
	/// Its bytes, to validate it again.
	pub bytes: Vec<u8>,
	/// A successful RPC reply, separate from inclusion.
	pub accepted: bool,
	/// Explicit RPC refusal.
	pub refused: bool,
	/// Send to submit reply; `None` when not timed (the reply came across a pause of PolkaMeter).
	pub reply_ms: Option<u64>,
	/// Send to first observed inclusion; `None` when not included yet, or [`FloodTx::untimed`].
	pub inclusion_ms: Option<u64>,
	/// Dispatch failure in the best block observation.
	pub failed_in_block: bool,
	/// Its time to inclusion is not known: timed across a pause of PolkaMeter, in flight on a
	/// sender connection that dropped, or read as the block follower caught up after a
	/// reconnect. It is left out of its step's latency, and of every export.
	pub untimed: bool,
}

#[derive(Debug)]
struct Request {
	hash: TxHash,
	kind: Kind,
	connection: usize,
	/// Since when it waits for a reply: when it was sent, or when its connection opened again.
	/// `None` while the connection is down, which is not the node's wait.
	waiting_since: Option<Millis>,
	/// Sent again after its connection dropped.
	resent: bool,
}

/// `author_submitExtrinsic` errors that mean the node already has the tx: the answer to a
/// request sent again after a dropped connection, when the node had read the first one.
const ALREADY_HAVE: [i64; 2] = [1012, 1013]; // temporarily banned, already imported

/// The tracker.
pub struct Tracker<S> {
	source: QueueSource,
	sender: S,
	load: SeriesWriter,
	blocks_out: JsonlWriter,
	sent: HashMap<TxHash, Sent>,
	requests: HashMap<u64, Request>,
	next_id: u64,
	/// The steps so far.
	pub steps: Vec<StepStats>,
	/// Every probe.
	pub probes: Vec<Probe>,
	/// Every flood tx sent.
	pub flood: HashMap<TxHash, FloodTx>,
	/// The block each included flood tx was seen in.
	pub included_in: HashMap<TxHash, u32>,
	/// The lowest block a flood tx can be in, where the loss check walks the finalized chain
	/// from: the one above the highest block finalized when the first flood tx was sent. Blocks
	/// up to that one were final before any flood tx existed. A block below the first one read
	/// can still have one: a fork built below it can win. `None` until a flood tx is sent.
	pub walk_from: Option<u32>,
	/// Blocks seen while recovering.
	pub recovery_blocks: Vec<BlockRecord>,
	/// The highest block with one of our txs.
	pub last_ours_block: u32,
	/// Last new best head: when, and the highest number.
	pub last_head: (Millis, u32),
	/// The highest block read so far (the drain waits for it to reach the last head).
	pub last_fetched: u32,
	/// Last new finalized block: when, and its number.
	pub last_finalized: (Millis, u32),
	/// What the follower could not read.
	pub unreadable: Vec<String>,
	/// Times the block follower lost the node and followed it again: not observed, so not a
	/// chain stall. Also sender connections that dropped and were opened again.
	pub observation_gaps: Vec<ObservationGap>,
	/// The node is down: the follower saw it restart.
	pub node_down: Option<String>,
	/// The node, or the network between, did not answer for the reconnect budget: the follower
	/// or a sender connection could not reach it. Node down only with evidence that it was the
	/// node ([`crate::load::runner::NodeProcess`]).
	pub unreachable: Option<String>,
	/// A connection to the node kept dropping, while the node did not restart: not the chain's
	/// failure, but no measure can be trusted, so the load ends on "generator limit".
	pub unstable: Option<String>,
	/// What the run should show but is not a problem: a reconnect that reached another node
	/// (a load balancer, or the node restarted).
	pub warnings: Vec<String>,
	/// The follower reconnected after losing the node at this time; the next new best block
	/// ends the catch-up.
	catching_up: Option<Millis>,
	/// Blocks seen in these windows (from, to] were read late, as the follower caught up after
	/// a reconnect: the time of their txs is not the chain's.
	read_late: Vec<(Millis, Millis)>,
	/// Probes in flight on a sender connection that dropped: when they land, they have no
	/// latency.
	untimed_probes: std::collections::HashSet<usize>,
	/// When PolkaMeter itself was paused, (from, to): every time measured across one is not
	/// known, and the stall clocks leave the time out.
	pauses: Vec<(Millis, Millis)>,
	phases: Vec<(Millis, Phase)>,
	baseline_probes: usize,
	/// Redacts RPC refusals before they are cut to their error key.
	redactor: crate::redact::Redactor,
}

/// Whether pause `p` lies across a time measured from `from` to `to`: it started before `to` and
/// ended after `from`. A suspend has no length on the run's clock, and is across a time that
/// spans it.
fn across(p: (Millis, Millis), from: Millis, to: Millis) -> bool {
	p.0 < to && p.1.max(p.0 + 1) > from
}

/// How much of `b` lies inside `a`.
fn overlap(a: (Millis, Millis), b: (Millis, Millis)) -> Millis {
	a.1.min(b.1).saturating_sub(a.0.max(b.0))
}

/// A stretch of the run the block follower did not observe, or a sender connection that
/// dropped: an RPC problem, not a chain failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservationGap {
	/// What lost the node: `blocks`, or `sender connection N`.
	pub what: String,
	/// When it was lost.
	pub from: Millis,
	/// When it was back; `None` while it is not.
	pub to: Option<Millis>,
	/// Why it was lost.
	pub reason: String,
}

impl std::fmt::Display for ObservationGap {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		match self.to {
			Some(to) => write!(
				f,
				"{}: RPC connection lost ({}); reconnected after {:.1} s, nothing observed meanwhile",
				self.what,
				self.reason,
				to.saturating_sub(self.from) as f64 / 1000.0
			),
			None => {
				write!(f, "{}: RPC connection lost ({}); not reconnected", self.what, self.reason)
			},
		}
	}
}

impl<S: Submit> Tracker<S> {
	/// A tracker sending the txs of `source` through `sender`, starting at `now`.
	pub fn new(
		source: QueueSource,
		sender: S,
		load: JsonlWriter,
		blocks_out: JsonlWriter,
		baseline_probes: usize,
		now: Millis,
	) -> Self {
		Self {
			source,
			sender,
			load: SeriesWriter::new(load),
			blocks_out,
			sent: HashMap::new(),
			requests: HashMap::new(),
			next_id: 1,
			steps: Vec::new(),
			probes: Vec::new(),
			flood: HashMap::new(),
			included_in: HashMap::new(),
			walk_from: None,
			recovery_blocks: Vec::new(),
			last_ours_block: 0,
			last_head: (now, 0),
			last_fetched: 0,
			last_finalized: (now, 0),
			unreadable: Vec::new(),
			observation_gaps: Vec::new(),
			node_down: None,
			unreachable: None,
			unstable: None,
			warnings: Vec::new(),
			catching_up: None,
			read_late: Vec::new(),
			untimed_probes: Default::default(),
			pauses: Vec::new(),
			phases: vec![(now, Phase::Baseline)],
			baseline_probes,
			redactor: crate::redact::Redactor::default(),
		}
	}

	/// Redacts the run's secrets from RPC refusals, whole, before they are cut to the error
	/// keys of the step records: a secret cut short would no longer be recognized.
	pub fn redact_with(&mut self, redactor: crate::redact::Redactor) {
		self.redactor = redactor;
	}

	/// The txs.
	pub fn source(&self) -> &QueueSource {
		&self.source
	}

	/// The sender.
	pub fn sender(&self) -> &S {
		&self.sender
	}

	pub(super) fn phase(&self) -> Phase {
		self.phases.last().expect("starts with baseline").1
	}

	pub(super) fn phase_at(&self, t: Millis) -> Phase {
		self.phases
			.iter()
			.rev()
			.find(|(at, _)| *at <= t)
			.map_or(Phase::Baseline, |p| p.1)
	}

	/// Moves to `phase`; writes `polkameter_phase` and `polkameter_step`.
	pub fn set_phase(&mut self, phase: Phase, now: Millis) -> Result<(), FileError> {
		self.phases.push((now, phase));
		if phase != Phase::Ramp {
			self.load.gauge(&STEP, [], -1.0, now)?;
		}
		for p in [Phase::Baseline, Phase::Ramp, Phase::Recovery, Phase::Done] {
			self.load.gauge(&PHASE, [p.name()], if p == phase { 1.0 } else { 0.0 }, now)?;
		}
		Ok(())
	}

	/// Starts step `k` with a target rate.
	pub fn start_step(&mut self, k: u32, rate: f64, now: Millis) -> Result<(), FileError> {
		self.steps.push(StepStats {
			step: k,
			target_rate: rate,
			started_at: now,
			ended_at: now,
			..StepStats::default()
		});
		self.load.gauge(&STEP, [], f64::from(k), now)
	}

	/// Ends the running step.
	pub fn end_step(&mut self, now: Millis) {
		if let Some(st) = self.steps.last_mut() {
			st.ended_at = now;
		}
	}

	/// The time from `from` to `to` that PolkaMeter did not observe the chain: the block
	/// follower was disconnected, or PolkaMeter itself was paused. The stall rules leave it out:
	/// no block could be seen then. Every other moment since the last new block counts, also
	/// across reconnects.
	pub fn unobserved_between(&self, from: Millis, to: Millis) -> Millis {
		let mut spans: Vec<(Millis, Millis)> = self
			.observation_gaps
			.iter()
			.filter(|g| g.what == "blocks")
			.map(|g| (g.from, g.to.unwrap_or(to)))
			.chain(self.pauses.iter().copied())
			.map(|(a, b)| (a.max(from), b.min(to)))
			.filter(|(a, b)| a < b)
			.collect();
		spans.sort_unstable();
		let (mut total, mut covered) = (0, from);
		for (a, b) in spans {
			total += b.saturating_sub(a.max(covered));
			covered = covered.max(b);
		}
		total
	}

	/// Why PolkaMeter did not observe all of `from`..`to`, if it did not: it was paused, a
	/// connection to the node was down, or a probe sent then landed without a time.
	pub fn unobserved_why(&self, from: Millis, to: Millis) -> Option<String> {
		if self.paused_across(from, to) {
			return Some(format!(
				"{} was paused for {} s of it (CPU starvation or a suspended machine)",
				crate::PRODUCT_NAME,
				crate::load::rules::secs(self.paused_within(from, to) as f64)
			));
		}
		if let Some(gap) = self
			.observation_gaps
			.iter()
			.find(|g| g.from < to && g.to.is_none_or(|back| back > from))
		{
			return Some(format!("{} lost the node for part of it: {gap}", crate::PRODUCT_NAME));
		}
		let untimed = |p: &&Probe| {
			(from..=to).contains(&p.sent_at)
				&& !matches!(p.outcome, Outcome::Pending | Outcome::Refused)
				&& p.latency_ms.is_none()
		};
		self.probes
			.iter()
			.find(untimed)
			.map(|_| format!("a probe landed that {} could not time", crate::PRODUCT_NAME))
	}

	/// What kept PolkaMeter from timing what it saw, for a warning: its own pauses, reconnects
	/// to the node, or both.
	pub fn untimed_cause(&self) -> &'static str {
		match (self.pauses.is_empty(), self.observation_gaps.is_empty()) {
			(false, true) => "it was paused",
			(true, _) => "it was reconnecting to the node",
			(false, false) => "it was paused, and reconnecting to the node",
		}
	}

	/// Whether PolkaMeter was paused across a time measured from `from` to `to`.
	pub(super) fn paused_across(&self, from: Millis, to: Millis) -> bool {
		self.pauses.iter().any(|p| across(*p, from, to))
	}

	/// How much of `from`..`to` PolkaMeter was paused.
	pub(super) fn paused_within(&self, from: Millis, to: Millis) -> Millis {
		self.pauses.iter().map(|p| overlap(*p, (from, to))).sum()
	}

	/// All the time the block follower was disconnected, up to `now`.
	pub fn unobserved_ms(&self, now: Millis) -> Millis {
		self.unobserved_between(0, now)
	}

	/// When the block follower lost the node, while it has not followed it again.
	fn unobserved_since(&self) -> Option<Millis> {
		self.observation_gaps
			.iter()
			.rev()
			.find(|g| g.what == "blocks" && g.to.is_none())
			.map(|g| g.from)
	}

	/// Sends one flood tx; false when it has none ready, every connection is backed up, or none
	/// is open. A tx that could not be queued goes back to the source and is the next one sent.
	pub fn send_next(&mut self, now: Millis) -> bool {
		let Some(tx) = self.source.next_tx() else {
			self.current().starved_ticks += 1;
			return false;
		};
		let step = self.steps.len() - 1;
		if !self.submit(tx.hash, &tx.bytes, Kind::Flood { step }, now) {
			self.source.put_back(tx);
			// Reconnecting is not backpressure: the node was not even reached.
			if self.sender.disconnected() {
				self.current().disconnected_ticks += 1;
			} else {
				self.current().backpressure_ticks += 1;
			}
			return false;
		}
		self.current().sent += 1;
		self.current().unknown_submissions += 1;
		self.load.inc(&TX_SENT, [CALL], 1.0, now);
		true
	}

	fn current(&mut self) -> &mut StepStats {
		self.steps.last_mut().expect("a step runs")
	}

	fn submit(&mut self, hash: TxHash, bytes: &[u8], kind: Kind, now: Millis) -> bool {
		let id = self.next_id;
		self.next_id += 1;
		let Some(connection) = self.sender.submit(id, bytes) else { return false };
		let waiting_since = Some(now);
		self.requests
			.insert(id, Request { hash, kind, connection, waiting_since, resent: false });
		if let Kind::Flood { step } = kind {
			self.walk_from.get_or_insert(self.last_finalized.1 + 1);
			let flood = FloodTx { step, sent_at: now, bytes: bytes.to_vec(), ..Default::default() };
			self.flood.insert(hash, flood);
		}
		self.sent.insert(hash, Sent { kind, sent_at: now });
		true
	}

	/// A reply or inclusion resolves a submission once, even if the other arrives later.
	fn observe_submission(&mut self, hash: &TxHash) {
		if let Some(tx) = self.flood.get(hash)
			&& !tx.accepted
			&& !tx.refused
			&& !self.included_in.contains_key(hash)
		{
			self.steps[tx.step].unknown_submissions -= 1;
		}
	}

	/// PolkaMeter itself was paused from `from` to `to` (SIGSTOP, CPU starvation, a suspend):
	/// blocks and replies that arrived meanwhile were read only after it. The runner records a
	/// pause before it hands on anything it read after it, so every time measured across it
	/// (latency, reply, block gap) is left out as it is read, and the stall clocks leave it out.
	pub fn paused(&mut self, from: Millis, to: Millis) {
		crate::redact::diag!(
			"{} was paused for {} s; nothing it measured across that time is timed",
			crate::PRODUCT_NAME,
			crate::load::rules::secs(to.saturating_sub(from) as f64)
		);
		self.pauses.push((from, to));
	}

	/// Flood txs sent and not yet included or refused.
	pub fn outstanding(&self) -> Vec<TxHash> {
		self.sent
			.iter()
			.filter(|(_, s)| matches!(s.kind, Kind::Flood { .. }))
			.map(|(h, _)| *h)
			.collect()
	}

	/// How long the oldest unanswered submit has waited, less the time PolkaMeter was paused.
	pub fn oldest_pending_ms(&self, now: Millis) -> Millis {
		self.requests
			.values()
			.filter_map(|r| r.waiting_since)
			.map(|since| now.saturating_sub(since).saturating_sub(self.paused_within(since, now)))
			.max()
			.unwrap_or(0)
	}

	/// Records the longest reply wait so far in the running step, and writes pending series once
	/// a second.
	pub fn tick(&mut self, now: Millis) -> Result<(), FileError> {
		let oldest = self.oldest_pending_ms(now);
		if let Some(st) = self.steps.last_mut() {
			st.max_oldest_pending_ms = st.max_oldest_pending_ms.max(oldest);
		}
		self.load.tick(now)
	}

	/// Submits sent and not yet answered.
	pub fn inflight(&self) -> usize {
		self.requests.len()
	}

	/// A submit reply. The reply time counts even when the tx was already included.
	pub fn on_reply(&mut self, r: Reply) {
		let (id, at, refusal) = match r {
			Reply::Accepted { id, at } => (id, at, None),
			Reply::Refused { id, at, code, error } => (id, at, Some((code, error))),
			Reply::Closed { connection, by_node } => return self.on_closed(connection, by_node),
			Reply::Interrupted { connection, ids, reason } => {
				return self.on_interrupted(connection, &ids, reason);
			},
			Reply::Reconnected { connection, down_ms } => {
				return self.on_reconnected(connection, down_ms);
			},
			Reply::Unstable { connection, reason } => {
				self.unstable.get_or_insert(format!("sender connection {connection}: {reason}"));
				return;
			},
		};
		let Some(req) = self.requests.remove(&id) else { return };
		let since = req.waiting_since.unwrap_or(at);
		// A reply read after a pause of PolkaMeter has no time: the pause is in it.
		let reply_ms = (!self.paused_across(since, at)).then(|| at.saturating_sub(since));
		if let (Kind::Flood { step }, Some(ms)) = (req.kind, reply_ms) {
			self.steps[step].reply_ms.push(ms);
		}
		if let Some(tx) = self.flood.get_mut(&req.hash) {
			tx.reply_ms = reply_ms;
		}
		// A refusal of a request sent again is no evidence: the node may have read the first
		// copy, and refuses this one for it (already imported, banned, or outdated once the
		// first is in a block). "Already have it" says the first one got through; anything else
		// leaves the tx to the blocks and the loss check.
		if req.resent
			&& let Some((code, _)) = &refusal
		{
			if ALREADY_HAVE.contains(code) {
				self.observe_submission(&req.hash);
				if let Some(tx) = self.flood.get_mut(&req.hash) {
					tx.accepted = true;
				}
			}
			return;
		}
		self.observe_submission(&req.hash);
		let Some((code, error)) = refusal else {
			if let Some(tx) = self.flood.get_mut(&req.hash) {
				tx.accepted = true;
			}
			return;
		};
		// Already included: the refusal is ignored, and does not flag the tx.
		if self.sent.remove(&req.hash).is_none() {
			return;
		}
		if let Some(tx) = self.flood.get_mut(&req.hash) {
			tx.refused = true;
		}
		match req.kind {
			Kind::Probe { index } => self.probes[index].outcome = Outcome::Refused,
			Kind::Flood { step } => {
				let st = &mut self.steps[step];
				st.rejected += 1;
				let error = self.redactor.text(format!("{code} {error}").trim());
				let key = crate::redact::truncate(&error, 160).trim_end().to_owned();
				*st.errors.entry(key).or_default() += 1;
				self.load.inc(&TX_REJECTED, [CALL, &code.to_string()], 1.0, at);
			},
		}
	}

	/// A sender connection dropped and is being opened again. Its unanswered submits (`ids`) are
	/// sent again on the new socket, and its queued ones wait for it: none of them waits on the
	/// node while the connection is down. When the node got each of them, before the drop or
	/// only after the reconnect, is unknown, so none of them is timed: a time from the first
	/// send would add the outage to the chain's latency, and one from the resend could hide it.
	fn on_interrupted(&mut self, connection: usize, ids: &[u64], reason: String) {
		for (id, r) in &mut self.requests {
			if r.connection == connection {
				r.waiting_since = None;
				r.resent |= ids.contains(id);
				match r.kind {
					Kind::Flood { .. } if !self.included_in.contains_key(&r.hash) => {
						if let Some(tx) = self.flood.get_mut(&r.hash) {
							tx.untimed = true;
						}
					},
					Kind::Flood { .. } => {},
					Kind::Probe { index } if self.probes[index].outcome == Outcome::Pending => {
						self.untimed_probes.insert(index);
					},
					Kind::Probe { .. } => {},
				}
			}
		}
		self.observation_gaps.push(ObservationGap {
			what: format!("sender connection {connection}"),
			from: crate::load::now_ms(),
			to: None,
			reason,
		});
	}

	/// The connection is open again: its requests wait on the node from now.
	fn on_reconnected(&mut self, connection: usize, down_ms: Millis) {
		let what = format!("sender connection {connection}");
		let Some(gap) = self
			.observation_gaps
			.iter_mut()
			.rev()
			.find(|g| g.what == what && g.to.is_none())
		else {
			return;
		};
		let back = gap.from + down_ms;
		gap.to = Some(back);
		for r in self.requests.values_mut().filter(|r| r.connection == connection) {
			r.waiting_since.get_or_insert(back);
		}
	}

	fn on_closed(&mut self, connection: usize, by_node: bool) {
		if by_node {
			self.unreachable.get_or_insert(format!(
				"sender connection {connection} could not be opened again within 15 s"
			));
		}
		// An interrupted request has an unknown outcome. Keep observing its hash;
		// calling it refused here would conceal possible chain inclusion or loss.
		self.requests.retain(|_, r| r.connection != connection);
	}

	/// Writes out the files and gives the sender back to be closed.
	pub fn finish(mut self) -> Result<S, FileError> {
		self.load.flush()?;
		self.blocks_out.finish()?;
		Ok(self.sender)
	}
}
