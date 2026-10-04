//! Follows finalized blocks for the recorders that must see every block (relay events,
//! maintenance calls). Finality can jump several blocks at once; the walker visits each one by
//! number, then calls `on_update` once with the newest finalized block (for state reads).
//!
//! A block that can't be read is a chain result: counted, in the problems once, and skipped.
//! An answer that doesn't decode as our types is our error and ends the walk. Nodes run with
//! `--state-pruning 256`, so a walker that falls more than 256 blocks behind loses blocks.

use std::future::Future;

use futures_util::FutureExt;
use polkameter_chain::{ChainError, Client, Fault};
use polkameter_files::Problems;
use tokio_util::sync::CancellationToken;

use crate::MonitorError;

/// A recorder the walker drives.
pub trait Walk: Send {
	/// Its name in the problems list.
	const NAME: &'static str;
	/// Every finalized block, in order.
	fn on_block(
		&mut self,
		number: u32,
		hash: [u8; 32],
	) -> impl Future<Output = Result<(), ChainError>> + Send;
	/// Once per finalized update, at the newest block.
	fn on_update(
		&mut self,
		_number: u32,
		_hash: [u8; 32],
	) -> impl Future<Output = Result<(), ChainError>> + Send {
		async { Ok(()) }
	}
	/// After the last block: what it could not record.
	fn finish(&mut self, _problems: &Problems) {}
}

struct Failures {
	name: &'static str,
	blocks: u32,
	updates: u32,
	problems: Problems,
}

impl Failures {
	/// A chain result: counted, told once. Our own error ends the walk.
	fn block(&mut self, n: u32, e: &ChainError) -> Result<(), MonitorError> {
		if e.fault() == Fault::Tool {
			return Err(MonitorError::Tool(format!("{}: block {n}: {e}", self.name)));
		}
		self.blocks += 1;
		if self.blocks == 1 {
			self.problems
				.record(format!("{}: block {n} could not be read ({e})", self.name));
		}
		Ok(())
	}

	fn update(&mut self, n: u32, e: &ChainError) -> Result<(), MonitorError> {
		if e.fault() == Fault::Tool {
			return Err(MonitorError::Tool(format!("{}: state at block {n}: {e}", self.name)));
		}
		self.updates += 1;
		if self.updates == 1 {
			self.problems
				.record(format!("{}: state at block {n} could not be read ({e})", self.name));
		}
		Ok(())
	}

	fn finish(&self) {
		if self.blocks > 1 {
			self.problems.record(format!(
				"{}: {} finalized blocks could not be read",
				self.name, self.blocks
			));
		}
		if self.updates > 1 {
			self.problems
				.record(format!("{}: {} state reads failed", self.name, self.updates));
		}
	}
}

/// Walks finalized blocks of `client` with `w` until `stop`; the block seen last is walked
/// before it returns. `Err` only for our own errors.
pub async fn walk<W: Walk>(
	client: Client,
	mut w: W,
	stop: CancellationToken,
	problems: Problems,
) -> Result<(), MonitorError> {
	let name = W::NAME;
	let mut blocks = match client.api().stream_blocks().await {
		Ok(b) => b,
		Err(e) => {
			problems.record(format!("{name}: cannot subscribe to finalized blocks ({e})"));
			return Ok(());
		},
	};
	let mut failures = Failures { name, blocks: 0, updates: 0, problems: problems.clone() };
	let mut last_done: Option<u32> = None;
	let mut ended = false;
	while !ended {
		let next = tokio::select! {
			() = stop.cancelled() => break,
			b = blocks.next() => b,
		};
		let Some(mut newest) = next else { break };
		// Heads that arrived meanwhile: every block is walked once, state is read at the newest.
		while let Some(more) = blocks.next().now_or_never() {
			match more {
				Some(b) => newest = b,
				None => {
					ended = true;
					break;
				},
			}
		}
		let (number, hash) = match newest {
			Ok(b) => (b.number() as u32, b.hash().0),
			Err(e) => {
				failures.block(
					last_done.map_or(0, |n| n + 1),
					&ChainError::Read { what: "finalized block", detail: e.to_string() },
				)?;
				continue;
			},
		};
		let from = last_done.map_or(number, |n| n + 1);
		for n in from..=number {
			let read = if n == number { Ok(hash) } else { client.block_hash(n).await };
			let r = match read {
				Ok(h) => w.on_block(n, h).await,
				Err(e) => Err(e),
			};
			if let Err(e) = r {
				failures.block(n, &e)?;
			}
		}
		if let Err(e) = w.on_update(number, hash).await {
			failures.update(number, &e)?;
		}
		last_done = Some(number);
	}
	if ended || !stop.is_cancelled() {
		problems.record(format!("{name}: the finalized block stream ended"));
	}
	failures.finish();
	w.finish(&problems);
	Ok(())
}
