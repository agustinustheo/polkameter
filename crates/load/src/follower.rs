//! Follows the chain's best and finalized blocks. Two tasks:
//! - heads: sends [`BlockEvent::Head`] the moment a new best block arrives (the stall rule reads
//!   it), and queues the block for the fetcher, after the blocks it builds on that were never
//!   read: after a reorg the node announces only the new tip, so the branch below it is
//!   filled in;
//! - fetcher: reads each queued block (body, weight, timestamp, events) strictly in arrival
//!   order and sends [`BlockEvent::Fetched`]. In order, so a source never sees a later block
//!   (and expires txs) before the inclusions of an earlier one.

use std::collections::HashSet;
use std::future::Future;

use polkameter_chain::{ChainError, Client, DecodeAsType, events, fetch, tx_hash};
use polkameter_files::{BlockRecord, Millis};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::now_ms;
use crate::source::TxHash;

/// What the follower saw.
#[derive(Debug)]
pub enum BlockEvent {
	/// A new best block arrived; not read yet.
	Head {
		/// Number.
		number: u32,
		/// Hash.
		hash: [u8; 32],
		/// When it arrived.
		seen_at: Millis,
	},
	/// A best block or a filled-in one, read. `record.ours` is for the tracker to fill in.
	Fetched {
		/// The block.
		record: BlockRecord,
		/// Its hash.
		hash: [u8; 32],
		/// Every extrinsic's hash, and whether it has an `ExtrinsicFailed` event.
		txs: Vec<(TxHash, bool)>,
	},
	/// A new finalized block.
	Finalized {
		/// Number.
		number: u32,
		/// When it arrived.
		seen_at: Millis,
	},
	/// A block could not be read: a result for the problems list, not a stop.
	Unreadable(String),
	/// An answer did not decode as our types: an error of our tools, the run stops.
	Tool(String),
}

#[derive(DecodeAsType)]
#[decode_as_type(crate_path = "polkameter_chain::scale_decode_reexport")]
struct Weight {
	ref_time: u64,
	proof_size: u64,
}

#[derive(DecodeAsType)]
#[decode_as_type(crate_path = "polkameter_chain::scale_decode_reexport")]
struct PerClass {
	normal: Weight,
	operational: Weight,
	mandatory: Weight,
}

/// The running runtime's normal class limit (ref time, proof size), from `System.BlockWeights`.
pub type Limit = (u64, u64);

/// A block for the fetcher: hash, number, when it arrived, filled in.
type Queued = ([u8; 32], u32, Millis, bool);

/// Blocks filled in below one best block at most; a longer walk is an unreadable stretch.
const MAX_FILL: usize = 64;

/// Starts both tasks; they end at `stop`.
pub fn start(
	client: Client,
	limit: Limit,
	out: mpsc::UnboundedSender<BlockEvent>,
	stop: CancellationToken,
) {
	let (queue_tx, queue) = mpsc::unbounded_channel();
	tokio::spawn(heads(client.clone(), out.clone(), queue_tx, stop.clone()));
	tokio::spawn(fetcher(client, limit, out, queue, stop));
}

async fn heads(
	client: Client,
	out: mpsc::UnboundedSender<BlockEvent>,
	queue: mpsc::UnboundedSender<Queued>,
	stop: CancellationToken,
) {
	let (Ok(mut best), Ok(mut finalized)) =
		(client.api().stream_best_blocks().await, client.api().stream_blocks().await)
	else {
		let _ = out.send(BlockEvent::Unreadable("cannot subscribe to blocks".into()));
		return;
	};
	// At start the stream goes back to the finalized block; record from the best one on, or
	// the first probes would be sent on old blocks and land "fast".
	let first = match client.best_number().await {
		Ok(n) => n,
		Err(e) => {
			let _ = out.send(BlockEvent::Unreadable(format!("best number: {e}")));
			return;
		},
	};
	let mut seen = HashSet::new();
	loop {
		tokio::select! {
			() = stop.cancelled() => return,
			block = finalized.next() => match block {
				Some(Ok(b)) => { let _ = out.send(BlockEvent::Finalized { number: b.number() as u32, seen_at: now_ms() }); }
				Some(Err(e)) => { let _ = out.send(BlockEvent::Unreadable(format!("finalized: {e}"))); }
				None => { let _ = out.send(BlockEvent::Unreadable("the finalized block stream ended".into())); return }
			},
			block = best.next() => match block {
				None => { let _ = out.send(BlockEvent::Unreadable("the best block stream ended".into())); return }
				Some(block) => match block {
				Ok(b) if b.number() as u32 >= first && !seen.contains(&b.hash().0) => {
					let (number, hash, seen_at) = (b.number() as u32, b.hash().0, now_ms());
					let _ = out.send(BlockEvent::Head { number, hash, seen_at });
					match fill_in(b.header().parent_hash.0, number, first, &seen, |h| client.parent(h)).await {
						Ok(missing) => for (h, n) in missing {
							seen.insert(h);
							let _ = queue.send((h, n, seen_at, true));
						},
						Err(e) => { let _ = out.send(BlockEvent::Unreadable(format!("blocks below {number}: {e}"))); }
					}
					seen.insert(hash);
					let _ = queue.send((hash, number, seen_at, false));
				}
				Ok(_) => {}
				Err(e) => { let _ = out.send(BlockEvent::Unreadable(format!("best: {e}"))); }
			}},
		}
	}
}

/// The blocks a new best block (`number`, on `parent`) builds on that are not in `seen`,
/// oldest first. The walk stops at a block in `seen` or at `first`, the first block followed.
async fn fill_in<F, Fut>(
	mut parent: [u8; 32],
	mut number: u32,
	first: u32,
	seen: &HashSet<[u8; 32]>,
	mut parent_of: F,
) -> Result<Vec<([u8; 32], u32)>, ChainError>
where
	F: FnMut([u8; 32]) -> Fut,
	Fut: Future<Output = Result<[u8; 32], ChainError>>,
{
	let mut missing = Vec::new();
	while number > first && !seen.contains(&parent) {
		if missing.len() == MAX_FILL {
			return Err(ChainError::Read {
				what: "filled-in blocks",
				detail: format!("no block read in the {MAX_FILL} below"),
			});
		}
		number -= 1;
		missing.push((parent, number));
		if number > first {
			parent = parent_of(parent).await?;
		}
	}
	missing.reverse();
	Ok(missing)
}

/// Reads up to 4 blocks at a time; results still come out in arrival order.
const FETCH_AHEAD: usize = 4;

async fn fetcher(
	client: Client,
	limit: Limit,
	out: mpsc::UnboundedSender<BlockEvent>,
	mut queue: mpsc::UnboundedReceiver<Queued>,
	stop: CancellationToken,
) {
	use futures_util::StreamExt;
	let blocks = futures_util::stream::poll_fn(move |cx| queue.poll_recv(cx));
	let mut reads = blocks
		.map(|(hash, number, seen_at, filled_in)| {
			let client = client.clone();
			async move {
				(
					hash,
					number,
					seen_at,
					filled_in,
					read(&client, hash, number, seen_at, limit).await,
				)
			}
		})
		.buffered(FETCH_AHEAD);
	let mut last: Option<(u64, Millis)> = None; // (timestamp, seen_at) of the last best block
	loop {
		let (hash, number, seen_at, filled_in, result) = tokio::select! {
			() = stop.cancelled() => return,
			Some(r) = reads.next() => r,
		};
		match result {
			Ok((mut record, txs)) => {
				record.fetch_lag_ms = Some(now_ms().saturating_sub(seen_at));
				record.filled_in = filled_in;
				if !filled_in {
					record.gap_ms = last.map(|(_, s)| seen_at.saturating_sub(s));
					record.interval_ms = last.map(|(t, _)| record.timestamp as i64 - t as i64);
					last = Some((record.timestamp, seen_at));
				}
				let _ = out.send(BlockEvent::Fetched { record, hash, txs });
			},
			Err(e) if e.fault() == polkameter_chain::Fault::Tool => {
				let _ = out.send(BlockEvent::Tool(format!("block {number}: {e}")));
			},
			Err(e) => {
				let _ = out.send(BlockEvent::Unreadable(format!("block {number}: {e}")));
			},
		}
	}
}

async fn read(
	client: &Client,
	hash: [u8; 32],
	number: u32,
	seen_at: Millis,
	limit: Limit,
) -> Result<(BlockRecord, Vec<(TxHash, bool)>), polkameter_chain::ChainError> {
	let at = client.at(hash).await?;
	let (body, weight, now, events) = tokio::join!(
		client.body(hash),
		fetch::<(), PerClass>(&at, "System", "BlockWeight", ()),
		fetch::<(), u64>(&at, "Timestamp", "Now", ()),
		events(&at),
	);
	let (body, timestamp) = (body?, now?.unwrap_or(0));
	let w = weight?.unwrap_or(PerClass {
		normal: Weight { ref_time: 0, proof_size: 0 },
		operational: Weight { ref_time: 0, proof_size: 0 },
		mandatory: Weight { ref_time: 0, proof_size: 0 },
	});
	let failed: HashSet<u32> = events?
		.into_iter()
		.filter(|e| e.0 == "System" && e.1 == "ExtrinsicFailed")
		.filter_map(|e| e.2)
		.collect();
	let pct = |v: u64, max: u64| if max == 0 { 0.0 } else { 100.0 * v as f64 / max as f64 };
	let record = BlockRecord {
		number,
		hash: format!("0x{}", hex::encode(hash)),
		seen_at,
		timestamp,
		extrinsics: body.len() as u32,
		bytes: body.iter().map(|x| x.len() as u64).sum(),
		normal_ref_time: w.normal.ref_time,
		normal_proof_size: w.normal.proof_size,
		normal_ref_time_pct: pct(w.normal.ref_time, limit.0),
		normal_proof_pct: pct(w.normal.proof_size, limit.1),
		operational_ref_time: w.operational.ref_time,
		mandatory_ref_time: w.mandatory.ref_time,
		..BlockRecord::default()
	};
	let txs = body
		.iter()
		.enumerate()
		.map(|(i, x)| (tx_hash(x), failed.contains(&(i as u32))))
		.collect();
	Ok((record, txs))
}

#[cfg(test)]
mod tests {
	use std::collections::HashMap;

	use super::*;

	fn h(n: u8) -> [u8; 32] {
		[n; 32]
	}

	/// Runs `fill_in` over a chain given as child -> parent, counting the parent reads.
	fn fill(
		chain: &HashMap<[u8; 32], [u8; 32]>,
		parent: [u8; 32],
		number: u32,
		first: u32,
		seen: &[[u8; 32]],
	) -> (Vec<([u8; 32], u32)>, usize) {
		let seen: HashSet<[u8; 32]> = seen.iter().copied().collect();
		let mut reads = 0;
		let out =
			futures_util::FutureExt::now_or_never(fill_in(parent, number, first, &seen, |x| {
				reads += 1;
				std::future::ready(
					chain
						.get(&x)
						.copied()
						.ok_or(ChainError::Read { what: "test", detail: "no parent".into() }),
				)
			}))
			.expect("ready")
			.expect("walks");
		(out, reads)
	}

	#[test]
	fn a_reorg_fills_in_the_new_branch() {
		// Read: 10 -> 11 -> 12. The node switches to 13' on 12' on 11' on 10, and announces 13'.
		let chain = HashMap::from([(h(111), h(10)), (h(112), h(111))]);
		let (out, reads) = fill(&chain, h(112), 13, 5, &[h(10), h(11), h(12)]);
		assert_eq!(out, [(h(111), 11), (h(112), 12)]);
		assert_eq!(reads, 2);
	}

	#[test]
	fn a_best_block_on_the_last_one_fills_in_nothing() {
		let (out, reads) = fill(&HashMap::new(), h(12), 13, 5, &[h(12)]);
		assert!(out.is_empty());
		assert_eq!(reads, 0);
	}

	#[test]
	fn the_walk_stops_at_the_first_block_followed() {
		// The first best block followed is 5: nothing below it is read, its parent included.
		let (out, _) = fill(&HashMap::new(), h(4), 5, 5, &[]);
		assert!(out.is_empty());
		let chain = HashMap::from([(h(6), h(5))]);
		let (out, reads) = fill(&chain, h(6), 7, 5, &[]);
		assert_eq!(out, [(h(5), 5), (h(6), 6)]);
		assert_eq!(reads, 1);
	}
}
