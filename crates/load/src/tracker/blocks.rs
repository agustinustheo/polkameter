//! Block events: our txs in each block read (best blocks and the ones filled in below them),
//! expiry, and one probe per best block. A tx counts as included the first time it is in a
//! block read, also when a reorg later replaces that block (it is then usually included again);
//! the loss check counts once more on the finalized chain.

use polkameter_files::registry::{TX_EXPIRED, TX_FAILED, TX_INCLUDED, TX_INCLUSION};
use polkameter_files::summary::{Outcome, Probe, ProbePhase};
use polkameter_files::{BlockRecord, FileError, Millis};

use super::{Phase, Tracker};
use crate::follower::BlockEvent;
use crate::source::{Block, Settled, TxHash};
use crate::submit::Submit;

impl<S: Submit> Tracker<S> {
	/// A block event. A new head moves the sources' birth block and sends one probe (baseline
	/// and recovery); a fetched block (best or filled in) settles our txs in it.
	pub fn on_block(&mut self, event: BlockEvent, now: Millis) -> Result<(), FileError> {
		match event {
			BlockEvent::Head { number, hash, seen_at } => {
				self.last_head = (seen_at, number.max(self.last_head.1));
				for lane in &mut self.lanes {
					lane.source.on_head(Block { number, hash });
				}
				if matches!(self.phase(), Phase::Baseline | Phase::Recovery) {
					self.send_probe(now);
				}
			},
			BlockEvent::Finalized { number, seen_at } if number > self.last_finalized.1 => {
				self.last_finalized = (seen_at, number)
			},
			BlockEvent::Finalized { .. } => {},
			BlockEvent::Unreadable(what) => self.unreadable.push(what),
			BlockEvent::Tool(_) => {}, // the runner stops on it before it gets here
			BlockEvent::Fetched { record, hash, txs } => {
				self.on_fetched(record, hash, &txs, now)?
			},
		}
		Ok(())
	}

	fn on_fetched(
		&mut self,
		mut record: BlockRecord,
		hash: [u8; 32],
		txs: &[(TxHash, bool)],
		now: Millis,
	) -> Result<(), FileError> {
		self.first_fetched.get_or_insert(record.number);
		let mut per_lane = vec![0u32; self.lanes.len()];
		for (tx, failed) in txs {
			if let Some(lane) = self.on_tx(tx, record.number, record.seen_at, *failed) {
				per_lane[lane] += 1;
				record.ours += 1;
				record.ours_failed += u32::from(*failed);
			}
		}
		record.finalized = self.last_finalized.1;
		self.blocks_out.write(&record)?;
		for lane in 0..self.lanes.len() {
			for tx in self.lanes[lane].source.on_block(Block { number: record.number, hash }) {
				self.expire(tx, now);
			}
		}
		self.last_fetched = self.last_fetched.max(record.number);
		match self.phase_at(record.seen_at) {
			Phase::Ramp => {
				for (lane, ours) in per_lane.iter().enumerate() {
					if let Some(st) =
						self.steps[lane].iter_mut().rev().find(|s| s.started_at <= record.seen_at)
					{
						st.blocks.push(BlockRecord { ours: *ours, ..record.clone() });
					}
				}
			},
			Phase::Recovery => self.recovery_blocks.push(record),
			_ => {},
		}
		Ok(())
	}

	fn on_tx(&mut self, hash: &TxHash, block: u32, seen_at: Millis, failed: bool) -> Option<usize> {
		let tx = self.sent.remove(hash)?;
		self.lanes[tx.lane].source.settled(hash, Settled::Included { failed });
		self.last_ours_block = self.last_ours_block.max(block);
		let latency = seen_at.saturating_sub(tx.sent_at);
		if let Some(p) = tx.probe {
			self.probes[p].outcome = if failed { Outcome::Failed } else { Outcome::Included };
			self.probes[p].latency_ms = Some(latency);
			return Some(tx.lane);
		}
		let call = self.lanes[tx.lane].call;
		self.included_in.insert(*hash, block);
		if let Some(tx) = self.flood.get_mut(hash) {
			tx.inclusion_ms = Some(latency);
			tx.failed_in_block = failed;
		}
		let st = &mut self.steps[tx.lane][tx.step.expect("flood tx")];
		st.included += 1;
		st.latencies_ms.push(latency);
		self.load.inc(&TX_INCLUDED, [call], 1.0, seen_at);
		self.load.observe(&TX_INCLUSION, [call], latency as f64 / 1000.0, seen_at);
		if failed {
			st.failed_in_block += 1;
			self.load.inc(&TX_FAILED, [call], 1.0, seen_at);
		} else {
			self.included_ok[tx.lane].push(*hash);
		}
		if self.phase() == Phase::Recovery {
			self.drained += 1;
		}
		Some(tx.lane)
	}

	fn expire(&mut self, hash: TxHash, now: Millis) {
		let Some(tx) = self.sent.remove(&hash) else { return };
		self.lanes[tx.lane].source.settled(&hash, Settled::Dropped);
		match (tx.probe, tx.step) {
			(Some(p), _) => self.probes[p].outcome = Outcome::Dropped,
			(None, Some(k)) => {
				if let Some(tx) = self.flood.get_mut(&hash) {
					tx.expired = true;
				}
				self.steps[tx.lane][k].dropped += 1;
				self.load.inc(&TX_EXPIRED, [self.lanes[tx.lane].call], 1.0, now);
			},
			(None, None) => {},
		}
	}

	fn send_probe(&mut self, now: Millis) {
		let phase = match self.phase() {
			Phase::Baseline if self.probes.len() < self.baseline_probes => ProbePhase::Baseline,
			Phase::Recovery => ProbePhase::Recovery,
			_ => return,
		};
		let Some(tx) = self.lanes[0].source.probe() else { return };
		let from = self.phases.last().expect("phase").0;
		self.probes.push(Probe {
			phase,
			sent_at_s: now.saturating_sub(from) as f64 / 1000.0,
			latency_ms: None,
			outcome: Outcome::Pending,
			sent_at: now,
		});
		let probe = self.probes.len() - 1;
		if !self.submit(0, tx.hash, &tx.bytes, None, Some(probe), now) {
			self.probes[probe].outcome = Outcome::Refused;
		}
	}
}
