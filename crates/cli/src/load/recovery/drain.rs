//! Recovery drain from the same finalized blocks that reconcile transaction loss.

use std::collections::{BTreeMap, HashMap};

use polkameter_plugin_sdk::observers::{Millis, summary::Recovery};

use crate::load::{source::TxHash, tracker::FloodTx};

/// Finalized inclusions and timestamps read by the loss check. Best blocks never enter here.
#[derive(Debug, Default)]
pub struct Canonical {
	/// The finalized block each flood transaction is in.
	pub inclusions: HashMap<TxHash, u32>,
	block_times: BTreeMap<u32, Millis>,
}

impl Canonical {
	/// Records a finalized block read by the loss check, including empty boundary blocks.
	/// A zero timestamp means that block's chain timestamp could not be read.
	pub fn record_block(
		&mut self,
		number: u32,
		timestamp: Millis,
		txs: impl IntoIterator<Item = TxHash>,
	) {
		self.block_times.insert(number, timestamp);
		self.inclusions.extend(txs.into_iter().map(|hash| (hash, number)));
	}

	/// Backlog at stop and time until its last finalized inclusion. Every transaction sent
	/// through stop needs a canonical inclusion timestamp; no best-block time can fill a gap.
	pub fn drain(
		&self,
		flood: &HashMap<TxHash, FloodTx>,
		stopped_at: Millis,
	) -> Result<(u64, Millis), &'static str> {
		let (mut backlog, mut last) = (0, stopped_at);
		for (hash, tx) in flood {
			if tx.sent_at > stopped_at {
				continue;
			}
			let block = self
				.inclusions
				.get(hash)
				.ok_or("a transaction's finalized inclusion is unknown")?;
			let at = self
				.block_times
				.get(block)
				.copied()
				.filter(|at| *at > 0)
				.ok_or("a finalized inclusion block has no chain timestamp")?;
			if at > stopped_at {
				backlog += 1;
				last = last.max(at);
			}
		}
		let boundary_known = self.block_times.iter().zip(self.block_times.iter().skip(1)).any(
			|((&n, &at), (&next, &after))| {
				n.checked_add(1) == Some(next) && at > 0 && at <= stopped_at && after > stopped_at
			},
		);
		if !boundary_known {
			return Err("no consecutive finalized block timestamps bracket load stop");
		}
		Ok((backlog, last - stopped_at))
	}

	/// Fills drain measures only after reconciliation. Missing evidence leaves nulls and a
	/// reason in the recovery detail, without changing the probe-based recovery result.
	pub fn apply_drain(
		&self,
		recovery: &mut Recovery,
		flood: &HashMap<TxHash, FloodTx>,
		stopped_at: Millis,
	) {
		recovery.backlog_at_stop = None;
		recovery.drained_seconds = None;
		recovery.drain_per_s = None;
		if !recovery.measured {
			return;
		}
		match self.drain(flood, stopped_at) {
			Ok((backlog, ms)) => {
				recovery.backlog_at_stop = Some(backlog);
				recovery.drained_seconds = Some(ms.div_ceil(1000));
				recovery.drain_per_s = (ms > 0).then(|| backlog as f64 / (ms as f64 / 1000.0));
			},
			Err(why) => recovery.detail.push_str(&format!("; drain not measured: {why}")),
		}
	}
}
