//! Every tx we sent, and what became of it. Plain code with no clock and no IO of its own
//! besides the two files it owns (`load.jsonl`, `blocks.jsonl`): the runner feeds it the time,
//! the send tick, the submit replies and the block events. Flood txs count towards the step they
//! were sent in; probes are apart. Block events are in [`blocks`].

mod blocks;

use std::collections::HashMap;

use polkameter_plugin_sdk::observers::{
	BlockRecord, FileError, JsonlWriter, Millis, SeriesWriter,
	registry::{PHASE, STEP, TX_REJECTED, TX_SENT},
	summary::{Outcome, Probe},
};

use crate::load::{plan::Lane, sender::Reply, source::TxHash, steps::StepStats, submit::Submit};

/// The phases of a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(missing_docs)]
pub enum Phase {
	Baseline,
	Ramp,
	Recovery,
	Done,
}

impl Phase {
	const ALL: [Phase; 4] = [Phase::Baseline, Phase::Ramp, Phase::Recovery, Phase::Done];

	/// The `phase` label.
	pub fn label(self) -> &'static str {
		match self {
			Phase::Baseline => "baseline",
			Phase::Ramp => "ramp",
			Phase::Recovery => "recovery",
			Phase::Done => "done",
		}
	}
}

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
	/// Reply and observed inclusion latency.
	pub reply_ms: Option<u64>,
	/// Send to first observed inclusion.
	pub inclusion_ms: Option<u64>,
	/// Dispatch failure in the best block observation.
	pub failed_in_block: bool,
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
	lane: Lane,
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
	/// The first block read.
	pub first_fetched: Option<u32>,
	/// Blocks seen while recovering.
	pub recovery_blocks: Vec<BlockRecord>,
	/// Flood txs included while recovering.
	pub drained: u64,
	/// The highest block with one of our txs.
	pub last_ours_block: u32,
	/// Connections the node closed.
	pub closed_by_node: usize,
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
	/// The node is down: the follower could not reach it within the reconnect backoff, or the
	/// connection kept dropping.
	pub node_down: Option<String>,
	/// What the run should show but is not a problem: a reconnect that reached another node
	/// (a load balancer, or the node restarted).
	pub warnings: Vec<String>,
	/// When the last new best block arrived while the follower was connected, for block gaps.
	prev_head_at: Option<Millis>,
	phases: Vec<(Millis, Phase)>,
	baseline_probes: usize,
	/// Redacts RPC refusals before they are cut to their error key.
	redactor: crate::redact::Redactor,
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
	/// A tracker sending `lane` through `sender`, starting at `now`.
	pub fn new(
		lane: Lane,
		sender: S,
		load: JsonlWriter,
		blocks_out: JsonlWriter,
		baseline_probes: usize,
		now: Millis,
	) -> Self {
		Self {
			lane,
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
			first_fetched: None,
			recovery_blocks: Vec::new(),
			drained: 0,
			last_ours_block: 0,
			closed_by_node: 0,
			last_head: (now, 0),
			last_fetched: 0,
			last_finalized: (now, 0),
			unreadable: Vec::new(),
			observation_gaps: Vec::new(),
			node_down: None,
			warnings: Vec::new(),
			prev_head_at: None,
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

	/// The txs and their `call` label.
	pub fn lane(&self) -> &Lane {
		&self.lane
	}

	/// The sender.
	pub fn sender(&self) -> &S {
		&self.sender
	}

	/// The sender, to change it (tests).
	pub fn sender_mut(&mut self) -> &mut S {
		&mut self.sender
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
		for p in Phase::ALL {
			self.load.gauge(&PHASE, [p.label()], if p == phase { 1.0 } else { 0.0 }, now)?;
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

	/// Ends the running step. The time since the last new best block counts as a block gap in
	/// the step now, so a stall at its end is seen in its own numbers.
	pub fn end_step(&mut self, now: Millis) {
		let (prev, unobserved) = (self.prev_head_at, self.unobserved_since());
		if let Some(st) = self.steps.last_mut() {
			st.ended_at = now;
			st.closed = true;
			if let Some(prev) = prev {
				st.gap_portion(prev, now);
			}
			if let Some(from) = unobserved {
				st.unobserved_ms += now.saturating_sub(from.max(st.started_at));
			}
		}
	}

	/// The time from `from` to `to` that the block follower was disconnected. The stall rules
	/// leave it out: no block could be seen then. Every other moment since the last new block
	/// counts, also across reconnects.
	pub fn unobserved_between(&self, from: Millis, to: Millis) -> Millis {
		self.observation_gaps
			.iter()
			.filter(|g| g.what == "blocks")
			.map(|g| g.to.unwrap_or(to).min(to).saturating_sub(g.from.max(from)))
			.sum()
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

	/// Sends one flood tx; false when it has none ready or every connection is backed up.
	/// A tx that could not be queued goes back to the source and is the next one sent.
	pub fn send_next(&mut self, now: Millis) -> bool {
		let Some(tx) = self.lane.source.next_tx() else {
			self.current().starved_ticks += 1;
			return false;
		};
		let step = self.steps.len() - 1;
		if !self.submit(tx.hash, &tx.bytes, Kind::Flood { step }, now) {
			self.lane.source.put_back(tx);
			self.current().backpressure_ticks += 1;
			return false;
		}
		self.current().sent += 1;
		self.load.inc(&TX_SENT, [self.lane.call], 1.0, now);
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
			let flood = FloodTx { step, sent_at: now, bytes: bytes.to_vec(), ..Default::default() };
			self.flood.insert(hash, flood);
		}
		self.sent.insert(hash, Sent { kind, sent_at: now });
		true
	}

	/// Flood txs sent and not yet included or refused.
	pub fn outstanding(&self) -> Vec<TxHash> {
		self.sent
			.iter()
			.filter(|(_, s)| matches!(s.kind, Kind::Flood { .. }))
			.map(|(h, _)| *h)
			.collect()
	}

	/// How long the oldest unanswered submit has waited.
	pub fn oldest_pending_ms(&self, now: Millis) -> Millis {
		self.requests
			.values()
			.filter_map(|r| r.waiting_since)
			.map(|since| now.saturating_sub(since))
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
		};
		let Some(req) = self.requests.remove(&id) else { return };
		let reply_ms = at.saturating_sub(req.waiting_since.unwrap_or(at));
		if let Kind::Flood { step } = req.kind {
			self.steps[step].reply_ms.push(reply_ms);
		}
		if let Some(tx) = self.flood.get_mut(&req.hash) {
			tx.reply_ms = Some(reply_ms);
		}
		// A request sent again that the node already has: the first one got through.
		let refusal = refusal.filter(|(code, _)| !(req.resent && ALREADY_HAVE.contains(code)));
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
				self.load.inc(&TX_REJECTED, [self.lane.call, &code.to_string()], 1.0, at);
			},
		}
	}

	/// A sender connection dropped and is being opened again. Its unanswered submits (`ids`) are
	/// sent again on the new socket, and its queued ones wait for it: none of them waits on the
	/// node while the connection is down.
	fn on_interrupted(&mut self, connection: usize, ids: &[u64], reason: String) {
		for (id, r) in &mut self.requests {
			if r.connection == connection {
				r.waiting_since = None;
				r.resent |= ids.contains(id);
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
		self.closed_by_node += usize::from(by_node);
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
