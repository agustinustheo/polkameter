//! Where a lane's txs come from. A scenario builds one per lane in `prepare`; the tracker owns
//! it, asks it for the next tx and tells it what happened to each one.

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

/// How a tx ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Settled {
	/// Seen in a best block; `failed` when it has an `ExtrinsicFailed` event.
	Included {
		/// Dispatch failed.
		failed: bool,
	},
	/// The node refused it, or it could not be sent.
	Rejected,
	/// Not in a block after its mortality ended.
	Dropped,
}

/// A best block, as a source sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Block {
	/// Number.
	pub number: u32,
	/// Hash (a mortal tx is born at a block).
	pub hash: [u8; 32],
}

/// A source of load txs.
pub trait LoadSource: Send {
	/// The next tx to send now; `None` when none is ready (not due yet, or used up).
	fn next(&mut self) -> Option<Tx>;
	/// A tx for a baseline or recovery probe, from a reserve `next` doesn't use.
	fn probe(&mut self) -> Option<Tx>;
	/// True when `next` will not return a tx again.
	fn exhausted(&self) -> bool;
	/// Why `next` returned nothing, for the stop detail.
	fn starved_reason(&self) -> String;
	/// A tx ended; a stateful source (coins) frees what it used.
	fn settled(&mut self, _hash: &TxHash, _how: Settled) {}
	/// A new best block arrived (before it is read): the moment to move a mortal tx's birth
	/// block forward.
	fn on_head(&mut self, _block: Block) {}
	/// Called for every best block once read, after its inclusions; returns txs that can no
	/// longer be included (their mortality ended).
	fn on_block(&mut self, _block: Block) -> Vec<TxHash> {
		Vec::new()
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
	/// `what` names the txs for the stop detail, e.g. "claims".
	pub fn new(flood: Vec<Tx>, probes: Vec<Tx>, what: &'static str) -> Self {
		let total = (flood.len(), probes.len());
		Self { flood: flood.into_iter(), probes: probes.into_iter(), what, total }
	}
}

impl LoadSource for QueueSource {
	fn next(&mut self) -> Option<Tx> {
		self.flood.next()
	}
	fn probe(&mut self) -> Option<Tx> {
		self.probes.next()
	}
	fn exhausted(&self) -> bool {
		self.flood.len() == 0
	}
	fn starved_reason(&self) -> String {
		format!("all {} {} sent ({} more kept for probes)", self.total.0, self.what, self.total.1)
	}
}

/// Probes a flood keeps: the baseline, one per block of recovery, and a margin.
pub fn probe_count(baseline: usize, recovery_s: u32, block_interval_s: f64) -> usize {
	baseline + (f64::from(recovery_s) / block_interval_s).ceil() as usize + 10
}
