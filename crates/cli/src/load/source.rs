//! Where the load's txs come from. The measurement builds the source from the prepared txs; the
//! tracker owns it and asks it for the next tx.

use std::sync::Arc;

/// blake2_256 of the tx bytes.
pub type TxHash = [u8; 32];

/// One tx, ready to send.
#[derive(Debug, Clone)]
pub struct Tx {
	/// blake2_256 of `bytes`.
	pub hash: TxHash,
	/// SCALE bytes as sent.
	pub bytes: Arc<[u8]>,
}

impl Tx {
	/// A tx and its hash.
	pub fn new(bytes: Vec<u8>) -> Self {
		Self { hash: polkameter_plugin_sdk::chain::tx_hash(&bytes), bytes: bytes.into() }
	}
}

/// Txs built ahead of time and sent in order, with their own probe txs. A tx handed out and not
/// queued (every connection backed up) comes back with [`QueueSource::put_back`] and is the next
/// one handed out, so backpressure never drops a tx or reorders the txs (nonces stay in order).
#[derive(Debug)]
pub struct QueueSource {
	flood: std::vec::IntoIter<Tx>,
	probes: std::vec::IntoIter<Tx>,
	held: Option<Tx>,
	held_probe: Option<Tx>,
	total: (usize, usize),
}

impl QueueSource {
	/// The flood txs, sent in order, and the probe txs.
	pub fn new(flood: Vec<Tx>, probes: Vec<Tx>) -> Self {
		let total = (flood.len(), probes.len());
		Self {
			flood: flood.into_iter(),
			probes: probes.into_iter(),
			held: None,
			held_probe: None,
			total,
		}
	}

	/// The next tx to send; `None` when used up.
	pub fn next_tx(&mut self) -> Option<Tx> {
		self.held.take().or_else(|| self.flood.next())
	}

	/// Gives back a tx from [`QueueSource::next_tx`] that could not be queued; it is sent next.
	pub fn put_back(&mut self, tx: Tx) {
		debug_assert!(self.held.is_none(), "one tx is handed out at a time");
		self.held = Some(tx);
	}

	/// A tx for a baseline or recovery probe, from a reserve `next_tx` doesn't use.
	pub fn probe(&mut self) -> Option<Tx> {
		self.held_probe.take().or_else(|| self.probes.next())
	}

	/// Gives back a probe tx that could not be queued; it is the next probe.
	pub fn put_back_probe(&mut self, tx: Tx) {
		debug_assert!(self.held_probe.is_none(), "one probe is handed out at a time");
		self.held_probe = Some(tx);
	}

	/// True when `next_tx` will not return a tx again.
	pub fn exhausted(&self) -> bool {
		self.held.is_none() && self.flood.len() == 0
	}

	/// Why `next_tx` returned nothing, for the stop detail.
	pub fn starved_reason(&self) -> String {
		format!(
			"all {} prepared transactions sent ({} more kept for probes)",
			self.total.0, self.total.1
		)
	}
}

/// Probes a flood keeps: the baseline, one per block of recovery, and a margin.
pub fn probe_count(baseline: usize, recovery_s: u32, block_interval_s: f64) -> usize {
	baseline + (f64::from(recovery_s) / block_interval_s).ceil() as usize + 10
}
