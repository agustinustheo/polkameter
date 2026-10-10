//! Where a lane's txs come from. A scenario builds one per lane in `prepare`; the tracker owns
//! it, asks it for the next tx.

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
		Self { hash: polkameter_chain::tx_hash(&bytes), bytes: bytes.into() }
	}
}

/// Txs built ahead of time and sent in order, with their own probe txs.
#[derive(Debug)]
pub struct QueueSource {
	flood: std::vec::IntoIter<Tx>,
	probes: std::vec::IntoIter<Tx>,
	what: &'static str,
	total: (usize, usize),
}

impl QueueSource {
	/// `what` names the txs for the stop detail, e.g. "prepared transactions".
	pub fn new(flood: Vec<Tx>, probes: Vec<Tx>, what: &'static str) -> Self {
		let total = (flood.len(), probes.len());
		Self { flood: flood.into_iter(), probes: probes.into_iter(), what, total }
	}

	/// The next tx to send; `None` when used up.
	pub fn next_tx(&mut self) -> Option<Tx> {
		self.flood.next()
	}

	/// A tx for a baseline or recovery probe, from a reserve `next_tx` doesn't use.
	pub fn probe(&mut self) -> Option<Tx> {
		self.probes.next()
	}

	/// True when `next_tx` will not return a tx again.
	pub fn exhausted(&self) -> bool {
		self.flood.len() == 0
	}

	/// Why `next_tx` returned nothing, for the stop detail.
	pub fn starved_reason(&self) -> String {
		format!("all {} {} sent ({} more kept for probes)", self.total.0, self.what, self.total.1)
	}
}

/// Probes a flood keeps: the baseline, one per block of recovery, and a margin.
pub fn probe_count(baseline: usize, recovery_s: u32, block_interval_s: f64) -> usize {
	baseline + (f64::from(recovery_s) / block_interval_s).ceil() as usize + 10
}
