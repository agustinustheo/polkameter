//! Every tx we sent, and what became of it. Plain code with no clock and no IO of its own
//! besides the two files it owns (`load.jsonl`, `blocks.jsonl`): the runner feeds it the time,
//! the send tick, the submit replies and the block events. Flood txs count towards the step and
//! lane they were sent in; probes are apart. Block events are in [`blocks`].

mod blocks;

use std::collections::HashMap;

use polkameter_files::{
	BlockRecord, FileError, JsonlWriter, Millis, SeriesWriter,
	registry::{PHASE, STEP, TX_REJECTED, TX_SENT},
	summary::{Outcome, Probe},
};

use crate::{plan::Lane, sender::Reply, source::TxHash, steps::StepStats, submit::Submit};

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
	/// A flood tx, counted in step `step` of its lane.
	Flood { step: usize },
	/// A baseline or recovery probe: `probes[index]`.
	Probe { index: usize },
}

#[derive(Debug)]
struct Sent {
	lane: usize,
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
	lane: usize,
	kind: Kind,
	sent_at: Millis,
	connection: usize,
}

/// The tracker.
pub struct Tracker<S> {
	lanes: Vec<Lane>,
	sender: S,
	load: SeriesWriter,
	blocks_out: JsonlWriter,
	sent: HashMap<TxHash, Sent>,
	requests: HashMap<u64, Request>,
	next_id: u64,
	/// Per lane, the steps so far.
	pub steps: Vec<Vec<StepStats>>,
	/// Every probe (lane 0 sends them).
	pub probes: Vec<Probe>,
	/// Included flood txs that did not fail, per lane, for the state check.
	pub included_ok: Vec<Vec<TxHash>>,
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
	phases: Vec<(Millis, Phase)>,
	baseline_probes: usize,
}

impl<S: Submit> Tracker<S> {
	/// A tracker sending `lanes` through `sender`, starting at `now`.
	pub fn new(
		lanes: Vec<Lane>,
		sender: S,
		load: JsonlWriter,
		blocks_out: JsonlWriter,
		baseline_probes: usize,
		now: Millis,
	) -> Self {
		let n = lanes.len();
		Self {
			lanes,
			sender,
			load: SeriesWriter::new(load),
			blocks_out,
			sent: HashMap::new(),
			requests: HashMap::new(),
			next_id: 1,
			steps: vec![Vec::new(); n],
			probes: Vec::new(),
			included_ok: vec![Vec::new(); n],
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
			phases: vec![(now, Phase::Baseline)],
			baseline_probes,
		}
	}

	/// The lanes.
	pub fn lanes(&self) -> &[Lane] {
		&self.lanes
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
		for p in Phase::ALL {
			self.load.gauge(&PHASE, [p.label()], if p == phase { 1.0 } else { 0.0 }, now)?;
		}
		Ok(())
	}

	/// Starts step `k` with a target rate per lane.
	pub fn start_step(&mut self, k: u32, rates: &[f64], now: Millis) -> Result<(), FileError> {
		for (lane, rate) in self.steps.iter_mut().zip(rates) {
			lane.push(StepStats {
				step: k,
				target_rate: *rate,
				started_at: now,
				ended_at: now,
				..StepStats::default()
			});
		}
		self.load.gauge(&STEP, [], f64::from(k), now)
	}

	/// Ends the running step.
	pub fn end_step(&mut self, now: Millis) {
		for lane in &mut self.steps {
			if let Some(st) = lane.last_mut() {
				st.ended_at = now;
			}
		}
	}

	/// Sends one flood tx of `lane`; false when it has none ready or every connection is backed up.
	pub fn send_next(&mut self, lane: usize, now: Millis) -> bool {
		let Some(tx) = self.lanes[lane].source.next_tx() else {
			self.current(lane).starved_ticks += 1;
			return false;
		};
		let step = self.steps[lane].len() - 1;
		if !self.submit(lane, tx.hash, &tx.bytes, Kind::Flood { step }, now) {
			self.current(lane).backpressure_ticks += 1;
			return false;
		}
		self.current(lane).sent += 1;
		self.load.inc(&TX_SENT, [self.lanes[lane].call], 1.0, now);
		true
	}

	fn current(&mut self, lane: usize) -> &mut StepStats {
		self.steps[lane].last_mut().expect("a step runs")
	}

	fn submit(&mut self, lane: usize, hash: TxHash, bytes: &[u8], kind: Kind, now: Millis) -> bool {
		let id = self.next_id;
		self.next_id += 1;
		let Some(connection) = self.sender.submit(id, bytes) else { return false };
		self.requests.insert(id, Request { hash, lane, kind, sent_at: now, connection });
		if let Kind::Flood { step } = kind {
			let flood = FloodTx { step, sent_at: now, bytes: bytes.to_vec(), ..Default::default() };
			self.flood.insert(hash, flood);
		}
		self.sent.insert(hash, Sent { lane, kind, sent_at: now });
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
		self.requests.values().map(|r| now.saturating_sub(r.sent_at)).max().unwrap_or(0)
	}

	/// Records the longest reply wait so far in each lane's running step, and writes pending
	/// series once a second.
	pub fn tick(&mut self, now: Millis) -> Result<(), FileError> {
		let oldest = self.oldest_pending_ms(now);
		for lane in &mut self.steps {
			if let Some(st) = lane.last_mut() {
				st.max_oldest_pending_ms = st.max_oldest_pending_ms.max(oldest);
			}
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
		};
		let Some(req) = self.requests.remove(&id) else { return };
		if let Kind::Flood { step } = req.kind {
			self.steps[req.lane][step].reply_ms.push(at.saturating_sub(req.sent_at));
		}
		if let Some(tx) = self.flood.get_mut(&req.hash) {
			tx.reply_ms = Some(at.saturating_sub(req.sent_at));
		}
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
				let st = &mut self.steps[req.lane][step];
				st.rejected += 1;
				let key: String = format!("{code} {error}").trim().chars().take(160).collect();
				*st.errors.entry(key).or_default() += 1;
				self.load.inc(
					&TX_REJECTED,
					[self.lanes[req.lane].call, &code.to_string()],
					1.0,
					at,
				);
			},
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
