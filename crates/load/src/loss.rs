//! The loss check, after recovery: every flood tx we sent must be included, refused, expired or
//! still ready in the node's pool; anything else is lost. Then it waits until the last block
//! with our txs is finalized, counts our txs again on the finalized chain (the tracker read best
//! blocks, which a reorg can replace), and lets the scenario check chain state for a sample.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use polkameter_chain::{Client, tx_hash};
use polkameter_files::FinalStep;
use polkameter_files::summary::{Finality, Loss, LostTx, NodePool, OnChain};

use crate::scenario::StateCheck;
use crate::source::TxHash;
use crate::tracker::FloodTx;

const STATE_SAMPLE: usize = 100;
/// Lost txs checked one by one for lost.jsonl; the rest are only counted.
const LOST_DETAILED: usize = 1000;

/// Everything the loss check needs from the run.
#[derive(Debug)]
pub struct Settled<'a> {
	/// Final step numbers of every lane.
	pub finals: &'a [FinalStep],
	/// Flood txs not included, refused or expired.
	pub outstanding: &'a [TxHash],
	/// Included flood txs that did not fail, in send order.
	pub included_ok: &'a [TxHash],
	/// The highest block with one of ours.
	pub last_ours_block: u32,
	/// Every flood tx sent.
	pub flood: &'a HashMap<TxHash, FloodTx>,
	/// The block the tracker saw each included flood tx in.
	pub included_in: &'a HashMap<TxHash, u32>,
	/// The first block the tracker read.
	pub first_fetched: Option<u32>,
}

/// Runs the loss check, and gives the lost txs for lost.jsonl. `node_pool` reads the node's own
/// mempool and ready counts from `/metrics` (the caller knows the node); it runs after the
/// finality wait, once our included txs are pruned. Chain errors become notes, never a stop.
#[allow(clippy::too_many_lines)]
pub async fn loss_check(
	client: &Client,
	run: Settled<'_>,
	node_pool: impl std::future::Future<Output = Option<NodePool>>,
	state: Option<&dyn StateCheck>,
	finality_wait_ms: u64,
) -> (Loss, Vec<LostTx>, Vec<serde_json::Value>) {
	let mut notes = Vec::new();
	let sum = |f: fn(&FinalStep) -> u64| run.finals.iter().map(f).sum::<u64>();
	let mut loss = Loss {
		sent: sum(|f| f.sent),
		included: sum(|f| f.included),
		failed_in_block: sum(|f| f.failed_in_block),
		refused: sum(|f| f.rejected),
		dropped: sum(|f| f.dropped),
		..Loss::default()
	};
	// Snapshot the pool first, then require finality through the best head observed
	// after listing it. A tx can leave the pool while we walk, but cannot disappear
	// between a late pool snapshot and an earlier chain cutoff.
	let (ready, cutoff) =
		reconciliation_snapshot(client.pending(), client.best_number(), run.last_ours_block).await;
	let ready = ready.map_err(|e| notes.push(format!("the pool could not be listed: {e}"))).ok();
	let cutoff = cutoff
		.map_err(|e| notes.push(format!("pool snapshot head unavailable: {e}")))
		.ok();
	let required_block = cutoff.unwrap_or(run.last_ours_block);
	let finalized = wait_finalized(client, required_block, finality_wait_ms).await;
	let mut on_chain = None;
	let mut finalized_failed = HashSet::new();
	match &finalized {
		Ok((number, _)) => {
			let waited_for_it = cutoff.is_some() && *number >= required_block;
			if !waited_for_it {
				notes.push(format!(
					"block {} not finalized after {} s (finalized {number})",
					required_block,
					finality_wait_ms / 1000
				));
			}
			loss.finalized = Some(Finality {
				last_ours_block: run.last_ours_block,
				finalized_at: *number,
				waited_for_it,
			});
			if let Some(first) = run.first_fetched {
				match finalized_txs(client, run.flood, first, *number).await {
					Ok((found, failed)) => {
						on_chain = Some((first, *number, found));
						finalized_failed = failed;
					},
					Err(e) => notes.push(format!("the finalized chain could not be walked: {e}")),
				}
			}
		},
		Err(e) => notes.push(format!("finalized block: {e}")),
	}
	if run.first_fetched.is_none() {
		notes.push("no block was read, so the finalized chain was not walked".into());
	}
	let r = reconcile(run.outstanding, run.included_in, on_chain.as_ref().map(|(_, _, f)| f));
	if let Some((first, last, found)) = &on_chain {
		loss.on_chain = OnChain {
			blocks: (*first, *last),
			included: found.len() as u64,
			missed: r.missed,
			only_on_fork: r.only_on_fork,
			moved: r.moved,
		};
	}
	let (in_pool, unaccounted): (Vec<_>, Vec<_>) = match &ready {
		Some(ready) => r.unaccounted.into_iter().partition(|(h, _)| ready.contains(h)),
		None => (Vec::new(), r.unaccounted),
	};
	// Lost needs both the pool and the finalized chain: without the walk, a tx the tracker
	// missed in a reorg would count as lost.
	let known = evidence_complete(
		ready.is_some(),
		on_chain.is_some(),
		loss.finalized.as_ref().is_some_and(|f| f.waited_for_it),
	);
	loss.accepted = run.flood.values().filter(|tx| tx.accepted).count() as u64;
	loss.unknown = unaccounted
		.iter()
		.filter(|(h, _)| run.flood.get(h).is_some_and(|tx| !tx.accepted))
		.count() as u64;
	let unaccounted: Vec<_> = unaccounted
		.into_iter()
		.filter(|(h, _)| run.flood.get(h).is_some_and(|tx| tx.accepted))
		.collect();
	if ready.is_some() {
		loss.in_pool = Some(in_pool.len() as u64);
	}
	if known {
		loss.lost = Some(unaccounted.len() as u64);
	}
	loss.node_pool = node_pool.await;
	let at = finalized.as_ref().ok().map(|(_, at)| *at);
	if let (Some(check), Some(at), false) = (state, at, run.included_ok.is_empty()) {
		let ok: Vec<_> = run
			.included_ok
			.iter()
			.filter(|hash| {
				on_chain.as_ref().is_some_and(|(_, _, found)| found.contains_key(*hash))
					&& !finalized_failed.contains(*hash)
			})
			.copied()
			.collect();
		let n = STATE_SAMPLE.min(ok.len());
		let sample: Vec<TxHash> = (0..n).map(|i| ok[i * ok.len() / n]).collect();
		if !sample.is_empty() {
			match check.check(client, &sample, at).await {
				Ok(s) => loss.state = Some(s),
				Err(e) => notes.push(format!("state check: {e}")),
			}
		}
	}

	if known && unaccounted.len() > LOST_DETAILED {
		notes.push(format!(
			"lost.jsonl has the first {LOST_DETAILED} of {} lost txs",
			unaccounted.len()
		));
	}
	let mut lost = Vec::new();
	for (hash, on_fork_block) in unaccounted.into_iter().take(if known { LOST_DETAILED } else { 0 })
	{
		let Some(tx) = run.flood.get(&hash) else { continue };
		let state_landed = match (state, at) {
			(Some(check), Some(at)) => {
				check.check(client, &[hash], at).await.ok().map(|s| s.missing == 0)
			},
			_ => None,
		};
		let validate = match client.validate(&tx.bytes, "lost tx").await {
			Ok(()) => "valid".to_owned(),
			Err(e) => e.to_string(),
		};
		lost.push(LostTx {
			hash: format!("0x{}", hex::encode(hash)),
			step: tx.step,
			sent_at: tx.sent_at,
			on_fork_block,
			scenario: state.map_or(serde_json::Value::Null, |s| s.describe(&hash)),
			state_landed,
			validate,
		});
	}
	lost.sort_by_key(|l| l.sent_at);
	loss.note = (!notes.is_empty()).then(|| notes.join("; "));
	let mut records = ledger_records(
		run.flood,
		run.included_in,
		on_chain.as_ref().map(|(_, _, found)| found),
		&finalized_failed,
		ready.as_ref(),
		cutoff,
		known,
	);
	// Include accepted transactions whose evidence is incomplete, not just
	// requests without an acceptance reply, in the unknown bucket.
	loss.unknown = records.iter().filter(|record| record["status"] == "unknown").count() as u64;
	records.sort_by_key(|record| record["sentAt"].as_u64());
	(loss, lost, records)
}

// The persisted ledger is the exclusive accounting view, including incomplete evidence.
fn ledger_records(
	flood: &HashMap<TxHash, FloodTx>,
	included_in: &HashMap<TxHash, u32>,
	on_chain: Option<&HashMap<TxHash, u32>>,
	finalized_failed: &HashSet<TxHash>,
	ready: Option<&HashSet<TxHash>>,
	cutoff: Option<u32>,
	known: bool,
) -> Vec<serde_json::Value> {
	flood
		.iter()
		.map(|(hash, tx)| {
			let finalized_block =
				on_chain.and_then(|found| found.get(hash)).copied();
			let ready = ready.map(|pool| pool.contains(hash));
			let status = classify(tx, finalized_block, ready, known);
			serde_json::json!({"version":1,"hash":hex::encode(hash),"step":tx.step,"sentAt":tx.sent_at,
            "poolSnapshotBlock":cutoff,"accepted":tx.accepted,"replyMs":tx.reply_ms,"inclusionMs":tx.inclusion_ms,
            "bestBlock":included_in.get(hash),"failedInBestBlock":tx.failed_in_block,
            "finalizedBlock":finalized_block,"failedInFinalizedBlock":finalized_block.map(|_|finalized_failed.contains(hash)),"inReadyPool":ready,"status":status})
		})
		.collect()
}

// The futures are deliberately awaited in this order, never joined concurrently.
async fn reconciliation_snapshot<E>(
	pool: impl std::future::Future<Output = Result<Vec<TxHash>, E>>,
	best: impl std::future::Future<Output = Result<u32, E>>,
	last_ours: u32,
) -> (Result<HashSet<TxHash>, E>, Result<u32, E>) {
	let ready = pool.await.map(|hashes| hashes.into_iter().collect());
	let cutoff = best.await.map(|number| number.max(last_ours));
	(ready, cutoff)
}

fn evidence_complete(pool: bool, chain: bool, finality: bool) -> bool {
	pool && chain && finality
}
fn classify(
	tx: &FloodTx,
	finalized: Option<u32>,
	ready: Option<bool>,
	complete: bool,
) -> &'static str {
	if finalized.is_some() {
		"finalized"
	} else if ready == Some(true) {
		"in_pool"
	} else if tx.refused {
		"refused"
	} else if tx.expired {
		"expired"
	} else if tx.accepted && complete {
		"lost"
	} else {
		"unknown"
	}
}

/// Our flood txs in the finalized blocks `first..=last`, with the block each is in.
async fn finalized_txs(
	client: &Client,
	flood: &HashMap<TxHash, FloodTx>,
	first: u32,
	last: u32,
) -> Result<(HashMap<TxHash, u32>, HashSet<TxHash>), polkameter_chain::ChainError> {
	let mut found = HashMap::new();
	let mut failed = HashSet::new();
	for n in first..=last {
		let hash = client.block_hash(n).await?;
		let body = client.body(hash).await?;
		let ours: Vec<_> = body
			.iter()
			.enumerate()
			.filter_map(|(index, x)| {
				let h = tx_hash(x);
				flood.contains_key(&h).then_some((index, h))
			})
			.collect();
		if ours.is_empty() {
			continue;
		}
		let events = polkameter_chain::events(&client.at(hash).await?).await?;
		for (index, h) in ours {
			found.insert(h, n);
			if events.iter().any(|event| {
				event.2 == Some(index as u32) && event.0 == "System" && event.1 == "ExtrinsicFailed"
			}) {
				failed.insert(h);
			}
		}
	}
	Ok((found, failed))
}

/// The tracker's view against the finalized chain.
#[derive(Debug, PartialEq)]
struct Reconciled {
	/// In a finalized block, never seen by the tracker.
	missed: u64,
	/// Seen by the tracker in a block the finalized chain doesn't have, and in no finalized block.
	only_on_fork: u64,
	/// Seen in one block, finalized in another.
	moved: u64,
	/// In no finalized block: each with the fork block the tracker saw it in, if any.
	unaccounted: Vec<(TxHash, Option<u32>)>,
}

/// Without `on_chain` (the walk failed) the tracker's view stands: the outstanding txs.
fn reconcile(
	outstanding: &[TxHash],
	included_in: &HashMap<TxHash, u32>,
	on_chain: Option<&HashMap<TxHash, u32>>,
) -> Reconciled {
	let Some(chain) = on_chain else {
		return Reconciled {
			missed: 0,
			only_on_fork: 0,
			moved: 0,
			unaccounted: outstanding.iter().map(|h| (*h, None)).collect(),
		};
	};
	let mut r = Reconciled { missed: 0, only_on_fork: 0, moved: 0, unaccounted: Vec::new() };
	for h in outstanding {
		if chain.contains_key(h) {
			r.missed += 1;
		} else {
			r.unaccounted.push((*h, None));
		}
	}
	for (h, seen) in included_in {
		match chain.get(h) {
			Some(n) if n != seen => r.moved += 1,
			Some(_) => {},
			None => {
				r.only_on_fork += 1;
				r.unaccounted.push((*h, Some(*seen)));
			},
		}
	}
	r
}

async fn wait_finalized(
	client: &Client,
	block: u32,
	finality_wait_ms: u64,
) -> Result<(u32, [u8; 32]), polkameter_chain::ChainError> {
	let deadline = crate::now_ms() + finality_wait_ms;
	loop {
		let at = client.finalized().await?;
		let number = at.block_number() as u32;
		if number >= block || crate::now_ms() > deadline {
			return Ok((number, at.block_hash().0));
		}
		tokio::time::sleep(Duration::from_secs(2)).await;
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn h(n: u8) -> TxHash {
		[n; 32]
	}

	#[test]
	fn accounting_ledger_statuses_are_exclusive() {
		#[derive(Clone, Copy)]
		enum State {
			Accepted,
			Refused,
			Expired,
			NoReply,
			BestBlock,
		}
		use State::*;
		let rows = [
			(Accepted, true, None, false, true, "in_pool"),
			(Accepted, false, None, false, true, "lost"),
			(Accepted, false, None, false, false, "unknown"),
			(NoReply, false, None, false, true, "unknown"),
			(BestBlock, false, None, false, true, "lost"),
			(BestBlock, false, Some(12), true, true, "finalized"),
			(Refused, false, None, false, true, "refused"),
			(Expired, false, None, false, true, "expired"),
		];
		let mut records = Vec::new();
		for (index, (state, in_pool, finalized, failed, complete, expected)) in
			rows.iter().enumerate()
		{
			let hash = h(index as u8);
			let tx = FloodTx {
				step: 0,
				sent_at: index as u64,
				bytes: vec![],
				accepted: matches!(state, Accepted | Expired | BestBlock),
				refused: matches!(state, Refused),
				expired: matches!(state, Expired),
				reply_ms: None,
				inclusion_ms: None,
				failed_in_block: false,
			};
			let best = if matches!(state, BestBlock) {
				HashMap::from([(hash, 10)])
			} else {
				HashMap::new()
			};
			let chain = finalized.map(|block| HashMap::from([(hash, block)])).unwrap_or_default();
			let pool = if *in_pool { HashSet::from([hash]) } else { HashSet::new() };
			let failures = if *failed { HashSet::from([hash]) } else { HashSet::new() };
			let row = ledger_records(
				&HashMap::from([(hash, tx)]),
				&best,
				Some(&chain),
				&failures,
				Some(&pool),
				Some(12),
				*complete,
			);
			assert_eq!(row.len(), 1);
			assert_eq!(row[0]["status"], *expected, "row {index}");
			assert_eq!(
				row[0]["failedInFinalizedBlock"],
				finalized
					.map(|_| *failed)
					.map_or(serde_json::Value::Null, serde_json::Value::Bool)
			);
			if matches!(state, BestBlock) && finalized.is_none() {
				assert_eq!(row[0]["bestBlock"], 10);
				assert!(row[0]["finalizedBlock"].is_null());
			}
			records.extend(row);
		}
		let statuses = ["finalized", "refused", "expired", "in_pool", "lost", "unknown"];
		let accounted: usize = statuses
			.iter()
			.map(|status| records.iter().filter(|r| r["status"] == *status).count())
			.sum();
		assert_eq!(accounted, rows.len(), "every sent hash has one accounting status");
		assert_eq!(
			records
				.iter()
				.map(|r| r["hash"].as_str().unwrap())
				.collect::<HashSet<_>>()
				.len(),
			rows.len(),
			"no hash is counted twice"
		);
	}

	#[test]
	fn a_reorg_moves_txs_between_the_counts() {
		// The tracker saw 1 and 2 in block 10 and 3 in block 11; 4 and 5 it never saw.
		let included_in = HashMap::from([(h(1), 10), (h(2), 10), (h(3), 11)]);
		let outstanding = [h(4), h(5)];
		// Finalized: block 10 was replaced by one with 1 and 4; 2 was included again in 12.
		let chain = HashMap::from([(h(1), 10), (h(4), 10), (h(2), 12)]);
		let r = reconcile(&outstanding, &included_in, Some(&chain));
		assert_eq!((r.missed, r.only_on_fork, r.moved), (1, 1, 1));
		let mut unaccounted = r.unaccounted;
		unaccounted.sort();
		assert_eq!(unaccounted, [(h(3), Some(11)), (h(5), None)]);
	}

	#[test]
	fn without_the_chain_the_tracker_view_stands() {
		let r = reconcile(&[h(4)], &HashMap::from([(h(1), 10)]), None);
		assert_eq!(r.unaccounted, [(h(4), None)]);
	}
}
