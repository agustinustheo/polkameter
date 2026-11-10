//! Block events: our txs in each block read (best blocks and the ones filled in below them), and
//! one probe per best block. A tx counts as included the first time it is in a block read, also
//! when a reorg later replaces that block (it is then usually included again); the loss check
//! counts once more on the finalized chain.

use polkameter_plugin_sdk::observers::{
	BlockRecord, FileError, Millis, Phase,
	registry::{TX_FAILED, TX_INCLUDED, TX_INCLUSION},
	summary::{Outcome, Probe, ProbePhase},
};

use super::{CALL, Kind, ObservationGap, Tracker};
use crate::load::{follower::BlockEvent, source::TxHash, submit::Submit};

impl<S: Submit> Tracker<S> {
	/// A block event. A new head sends one probe (baseline and recovery); a fetched block (best
	/// or filled in) settles our txs in it.
	pub fn on_block(&mut self, event: BlockEvent, now: Millis) -> Result<(), FileError> {
		match event {
			BlockEvent::Head { number, seen_at } => {
				if let Some(lost) = self.catching_up.take() {
					// This block and the ones filled in below it, read with it, are read late.
					self.read_late.push((lost, seen_at));
				}
				self.last_head = (seen_at, number.max(self.last_head.1));
				self.on_head(seen_at);
				if matches!(self.phase(), Phase::Baseline | Phase::Recovery) {
					self.send_probe(now);
				}
			},
			BlockEvent::Disconnected { at, reason } => {
				// What passed since the last block, while connected, was a gap; after it, nothing
				// was observed.
				if let Some(prev) = self.prev_head_at.take() {
					self.gap_until(prev, at);
				}
				self.observation_gaps.push(ObservationGap {
					what: "blocks".into(),
					from: at,
					to: None,
					reason,
				});
			},
			BlockEvent::Reconnected { at } => {
				let Some(from) = self.unobserved_since() else { return Ok(()) };
				self.catching_up = Some(from);
				if let Some(gap) = self
					.observation_gaps
					.iter_mut()
					.rev()
					.find(|g| g.what == "blocks" && g.to.is_none())
				{
					gap.to = Some(at);
				}
				// The stall rules keep the time since the last new block, less the time not
				// observed (`unobserved_between`): a reconnect does not restart them.
				self.prev_head_at = Some(at);
			},
			BlockEvent::Unreachable(why) | BlockEvent::Restarted(why) => {
				self.node_down.get_or_insert(why);
			},
			BlockEvent::Unstable(why) => {
				self.unstable.get_or_insert(why);
			},
			// Not node down: behind a load balancer the new node may be healthy. A chain that is
			// really down still trips the stall and finality stall rules.
			BlockEvent::OtherNode(why) => {
				if !self.warnings.contains(&why) {
					self.warnings.push(why);
				}
			},
			BlockEvent::Finalized { number, seen_at } if number > self.last_finalized.1 => {
				self.last_finalized = (seen_at, number)
			},
			BlockEvent::Finalized { .. } => {},
			BlockEvent::Unreadable(what) => self.unreadable.push(what),
			BlockEvent::Tool(_) => {}, // the runner stops on it before it gets here
			BlockEvent::Fetched { record, txs } => self.on_fetched(record, &txs)?,
		}
		Ok(())
	}

	fn on_fetched(
		&mut self,
		mut record: BlockRecord,
		txs: &[(TxHash, bool)],
	) -> Result<(), FileError> {
		// A gap PolkaMeter was paused in is not known: the block may have come at any time.
		if let Some(gap) = record.gap_ms
			&& self.paused_across(record.seen_at.saturating_sub(gap), record.seen_at)
		{
			record.gap_ms = None;
		}
		for (tx, failed) in txs {
			if self.on_tx(tx, record.number, record.seen_at, *failed) {
				record.ours += 1;
				record.ours_failed += u32::from(*failed);
			}
		}
		record.finalized = self.last_finalized.1;
		self.blocks_out.write(&record)?;
		self.last_fetched = self.last_fetched.max(record.number);
		match self.phase_at(record.seen_at) {
			Phase::Ramp => {
				if let Some(st) =
					self.steps.iter_mut().rev().find(|s| s.started_at <= record.seen_at)
				{
					st.blocks.push(record);
				}
			},
			Phase::Recovery => self.recovery_blocks.push(record),
			_ => {},
		}
		Ok(())
	}

	/// A new best block at `at`: the gap since the one before goes to the steps it spans, each
	/// the part inside its own time, and the block counts in the step it arrived in.
	fn on_head(&mut self, at: Millis) {
		if let Some(prev) = self.prev_head_at {
			self.gap_until(prev, at);
		}
		self.prev_head_at = Some(at);
		if let Some(st) = self.steps.iter_mut().rev().find(|s| s.started_at <= at)
			&& (!st.closed || at < st.ended_at)
		{
			st.heads += 1;
		}
	}

	/// No new best block from `from` to `to`: each step gets the part of it inside its time,
	/// less the time PolkaMeter was paused.
	pub(super) fn gap_until(&mut self, from: Millis, to: Millis) {
		for i in (0..self.steps.len()).rev() {
			let st = &self.steps[i];
			if st.closed && st.ended_at <= from {
				break;
			}
			let (start, end) =
				(from.max(st.started_at), if st.closed { to.min(st.ended_at) } else { to });
			let paused = self.paused_within(start, end);
			self.steps[i].gap_portion(from, to, paused);
		}
	}

	/// Whether `hash` is one of ours.
	fn on_tx(&mut self, hash: &TxHash, block: u32, seen_at: Millis, failed: bool) -> bool {
		let Some(tx) = self.sent.remove(hash) else { return false };
		self.last_ours_block = self.last_ours_block.max(block);
		let latency = seen_at.saturating_sub(tx.sent_at);
		let read_late = self.read_late.iter().any(|(from, to)| *from < seen_at && seen_at <= *to);
		let late = read_late || self.paused_across(tx.sent_at, seen_at);
		let step = match tx.kind {
			Kind::Probe { index } => {
				self.probes[index].outcome =
					if failed { Outcome::Failed } else { Outcome::Included };
				let untimed = late || self.untimed_probes.contains(&index);
				self.probes[index].latency_ms = (!untimed).then_some(latency);
				return true;
			},
			Kind::Flood { step } => step,
		};
		self.included_in.insert(*hash, block);
		let mut untimed = late;
		if let Some(flood) = self.flood.get_mut(hash) {
			flood.failed_in_block = failed;
			flood.untimed |= late;
			untimed = flood.untimed;
			flood.inclusion_ms = (!untimed).then_some(latency);
		}
		let st = &mut self.steps[step];
		st.included += 1;
		self.load.inc(&TX_INCLUDED, [CALL], 1.0, seen_at);
		if untimed {
			st.untimed += 1;
		} else {
			st.latencies_ms.push(latency);
			self.load.observe(&TX_INCLUSION, [CALL], latency as f64 / 1000.0, seen_at);
		}
		if failed {
			st.failed_in_block += 1;
			self.load.inc(&TX_FAILED, [CALL], 1.0, seen_at);
		}
		// The recovery drains what blocks announced after the load stopped include: not a block
		// announced in the load and read later, nor one read as the follower caught up.
		if !read_late && self.phase_at(seen_at) == Phase::Recovery {
			self.drained += 1;
		}
		true
	}

	fn send_probe(&mut self, now: Millis) {
		let phase = match self.phase() {
			Phase::Baseline if self.probes.len() < self.baseline_probes => ProbePhase::Baseline,
			Phase::Recovery => ProbePhase::Recovery,
			_ => return,
		};
		let Some(tx) = self.source.probe() else { return };
		let from = self.phases.last().expect("phase").0;
		self.probes.push(Probe {
			phase,
			sent_at_s: now.saturating_sub(from) as f64 / 1000.0,
			latency_ms: None,
			outcome: Outcome::Pending,
			sent_at: now,
		});
		let index = self.probes.len() - 1;
		if !self.submit(tx.hash, &tx.bytes, Kind::Probe { index }, now) {
			// Every connection is backed up: our side, not the node's answer. The probe was not
			// sent; its tx is kept for the next block.
			self.probes.pop();
			self.source.put_back_probe(tx);
		}
	}
}
