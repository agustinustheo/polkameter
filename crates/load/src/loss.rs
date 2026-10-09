//! The loss check, after recovery. Every flood tx we sent gets one [`Status`] from its own
//! evidence, and every printed count is tallied from those statuses, so they partition `sent`.
//! It waits until the last block with our txs is finalized, walks the finalized chain for them,
//! and lets the scenario check chain state for a sample.

use std::{
	collections::{HashMap, HashSet},
	time::Duration,
};

use polkameter_chain::{Client, tx_hash};
use polkameter_files::summary::{Finality, Loss, LostTx, NodePool, OnChain};
use serde::Serialize;

use crate::{scenario::StateCheck, source::TxHash, tracker::FloodTx};

const STATE_SAMPLE: usize = 100;
/// Lost txs checked one by one for lost.jsonl; the rest are only counted.
const LOST_DETAILED: usize = 1000;

/// Everything the loss check needs from the run.
#[derive(Debug)]
pub struct Settled<'a> {
	/// Every flood tx sent.
	pub flood: &'a HashMap<TxHash, FloodTx>,
	/// The block the tracker saw each included flood tx in.
	pub included_in: &'a HashMap<TxHash, u32>,
	/// Included flood txs that did not fail, in send order.
	pub included_ok: &'a [TxHash],
	/// The highest block with one of ours.
	pub last_ours_block: u32,
	/// The first block the tracker read.
	pub first_fetched: Option<u32>,
}

/// Runs the loss check, and gives the lost txs for lost.jsonl and the ledger rows. `node_pool`
/// reads the node's own mempool and ready counts from `/metrics`; it runs after the finality
/// wait, once our included txs are pruned. Chain errors become notes, never a stop.
#[allow(clippy::too_many_lines)]
pub async fn loss_check(
	client: &Client,
	run: Settled<'_>,
	node_pool: impl std::future::Future<Output = Option<NodePool>>,
	state: Option<&dyn StateCheck>,
	finality_wait_ms: u64,
) -> (Loss, Vec<LostTx>, Vec<serde_json::Value>) {
	let mut notes = Vec::new();
	let mut loss = Loss { sent: run.flood.len() as u64, ..Loss::default() };
	// Snapshot the pool first, then require finality through the best head observed
	// after listing it. A tx can leave the pool while we walk, but cannot disappear
	// between a late pool snapshot and an earlier chain cutoff. The reads are awaited in
	// this order, never joined concurrently.
	let ready = client.pending().await.map(|hashes| hashes.into_iter().collect::<HashSet<_>>());
	let ready = ready.map_err(|e| notes.push(format!("the pool could not be listed: {e}"))).ok();
	let cutoff = client.best_number().await.map(|number| number.max(run.last_ours_block));
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
	// Lost needs both the pool and the finalized chain: without the walk, a tx the tracker
	// missed in a reorg would count as lost.
	let known = ready.is_some()
		&& on_chain.is_some()
		&& loss.finalized.as_ref().is_some_and(|f| f.waited_for_it);
	let found = on_chain.as_ref().map(|(_, _, found)| found);
	let rows =
		ledger(run.flood, run.included_in, found, &finalized_failed, ready.as_ref(), cutoff, known);
	let count = |status: Status| rows.iter().filter(|r| r.status == status).count() as u64;
	loss.accepted = run.flood.values().filter(|tx| tx.accepted).count() as u64;
	loss.included = count(Status::Finalized);
	loss.refused = count(Status::Refused);
	loss.unverified = count(Status::Unverified);
	loss.unknown = count(Status::Unknown);
	loss.in_pool = ready.is_some().then(|| count(Status::InPool));
	loss.lost = known.then(|| count(Status::Lost));
	loss.failed_in_block = rows.iter().filter(|r| r.failed).count() as u64;
	if let Some((first, last, _)) = &on_chain {
		loss.on_chain = OnChain {
			blocks: (*first, *last),
			included: loss.included,
			missed: rows.iter().filter(|r| r.finalized.is_some() && r.best.is_none()).count()
				as u64,
			only_on_fork: rows.iter().filter(|r| r.finalized.is_none() && r.best.is_some()).count()
				as u64,
			moved: rows
				.iter()
				.filter(|r| matches!((r.best, r.finalized), (Some(b), Some(f)) if b != f))
				.count() as u64,
		};
	}
	loss.node_pool = node_pool.await;
	let at = finalized.as_ref().ok().map(|(_, at)| *at);
	if let (Some(check), Some(at), false) = (state, at, run.included_ok.is_empty()) {
		let ok: Vec<_> = run
			.included_ok
			.iter()
			.filter(|hash| {
				found.is_some_and(|f| f.contains_key(*hash)) && !finalized_failed.contains(*hash)
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
	let lost_rows: Vec<&Row> = rows.iter().filter(|r| r.status == Status::Lost).collect();
	if lost_rows.len() > LOST_DETAILED {
		notes.push(format!(
			"lost.jsonl has the first {LOST_DETAILED} of {} lost txs",
			lost_rows.len()
		));
	}
	let mut lost = Vec::new();
	for row in lost_rows.iter().take(LOST_DETAILED) {
		let state_landed = match (state, at) {
			(Some(check), Some(at)) => {
				check.check(client, &[row.hash], at).await.ok().map(|s| s.missing == 0)
			},
			_ => None,
		};
		let validate = match client.validate(&row.tx.bytes, "lost tx").await {
			Ok(()) => "valid".to_owned(),
			Err(e) => e.to_string(),
		};
		lost.push(LostTx {
			hash: polkameter_chain::hex0x(row.hash),
			step: row.tx.step,
			sent_at: row.tx.sent_at,
			on_fork_block: row.best,
			scenario: serde_json::Value::Null,
			state_landed,
			validate,
		});
	}
	lost.sort_by_key(|l| l.sent_at);
	loss.note = (!notes.is_empty()).then(|| notes.join("; "));
	let mut records: Vec<serde_json::Value> = rows.into_iter().map(|r| r.json).collect();
	records.sort_by_key(|record| record["sentAt"].as_u64());
	(loss, lost, records)
}

/// Where a flood tx ended, as `transactions.jsonl` writes it: exclusive, the first that holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
enum Status {
	/// In a finalized block the walk read.
	Finalized,
	/// Still ready in the node's pool.
	InPool,
	/// Refused at submit.
	Refused,
	/// Accepted or seen in a best block, in no finalized block, not ready: with the chain and the
	/// pool read, it is gone.
	Lost,
	/// Seen in a best block, but finality was not verified: the wait timed out or the chain was
	/// not walked.
	Unverified,
	/// No evidence either way: no reply and no inclusion, or the chain or pool was not read.
	Unknown,
}

fn classify(
	tx: &FloodTx,
	best: Option<u32>,
	finalized: Option<u32>,
	ready: Option<bool>,
	known: bool,
) -> Status {
	if finalized.is_some() {
		Status::Finalized
	} else if ready == Some(true) {
		Status::InPool
	} else if tx.refused {
		Status::Refused
	} else if known && (tx.accepted || best.is_some()) {
		Status::Lost
	} else if best.is_some() {
		Status::Unverified
	} else {
		Status::Unknown
	}
}

/// One flood tx's ledger row: its status, the evidence behind it, and the row as it is written.
struct Row<'a> {
	hash: TxHash,
	tx: &'a FloodTx,
	status: Status,
	best: Option<u32>,
	finalized: Option<u32>,
	failed: bool,
	json: serde_json::Value,
}

/// One row per flood tx. `found` is the finalized walk (`None` when it did not run).
fn ledger<'a>(
	flood: &'a HashMap<TxHash, FloodTx>,
	included_in: &HashMap<TxHash, u32>,
	found: Option<&HashMap<TxHash, u32>>,
	finalized_failed: &HashSet<TxHash>,
	ready: Option<&HashSet<TxHash>>,
	cutoff: Option<u32>,
	known: bool,
) -> Vec<Row<'a>> {
	flood
		.iter()
		.map(|(hash, tx)| {
			let best = included_in.get(hash).copied();
			let finalized = found.and_then(|f| f.get(hash)).copied();
			let ready = ready.map(|pool| pool.contains(hash));
			let status = classify(tx, best, finalized, ready, known);
			let failed_finalized = finalized.map(|_| finalized_failed.contains(hash));
			let json = serde_json::json!({"version":1,"hash":hex::encode(hash),"step":tx.step,
				"sentAt":tx.sent_at,"poolSnapshotBlock":cutoff,"accepted":tx.accepted,
				"replyMs":tx.reply_ms,"inclusionMs":tx.inclusion_ms,"bestBlock":best,
				"failedInBestBlock":tx.failed_in_block,"finalizedBlock":finalized,
				"failedInFinalizedBlock":failed_finalized,"inReadyPool":ready,"status":status});
			Row {
				hash: *hash,
				tx,
				status,
				best,
				finalized,
				failed: tx.failed_in_block || failed_finalized == Some(true),
				json,
			}
		})
		.collect()
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
		let failed_in_block = polkameter_chain::failed_extrinsics(&events);
		for (index, h) in ours {
			found.insert(h, n);
			if failed_in_block.contains(&(index as u32)) {
				failed.insert(h);
			}
		}
	}
	Ok((found, failed))
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
	use Status::*;

	/// A flood tx of a scenario: its reply, the best block the tracker saw it in, the finalized
	/// block that has it, whether it is ready in the pool, and the status it must get.
	type Case = (bool, bool, Option<u32>, Option<u32>, bool, Status);

	/// Runs the ledger over one scenario and checks each status, and that the statuses partition
	/// the sent txs.
	fn check(name: &str, walked: bool, known: bool, pool_listed: bool, cases: &[Case]) {
		let mut flood = HashMap::new();
		let mut included_in = HashMap::new();
		let mut found = HashMap::new();
		let mut ready = HashSet::new();
		for (index, &(accepted, refused, best, finalized, in_pool, _)) in cases.iter().enumerate() {
			let hash = [index as u8; 32];
			let tx = FloodTx { sent_at: index as u64, accepted, refused, ..Default::default() };
			flood.insert(hash, tx);
			if let Some(block) = best {
				included_in.insert(hash, block);
			}
			if let (true, Some(block)) = (walked, finalized) {
				found.insert(hash, block);
			}
			if in_pool && pool_listed {
				ready.insert(hash);
			}
		}
		let on_chain = walked.then_some(&found);
		let pool = pool_listed.then_some(&ready);
		let rows = ledger(&flood, &included_in, on_chain, &HashSet::new(), pool, Some(12), known);
		for row in &rows {
			let want = cases[row.tx.sent_at as usize].5;
			assert_eq!(row.status, want, "{name}: tx {}", row.tx.sent_at);
		}
		let total: u64 = [Finalized, InPool, Refused, Lost, Unverified, Unknown]
			.iter()
			.map(|s| rows.iter().filter(|r| r.status == *s).count() as u64)
			.sum();
		assert_eq!(total, flood.len() as u64, "{name}: statuses partition sent");
	}

	#[test]
	fn statuses_partition_sent_in_every_scenario() {
		// Normal: the walk and the pool both read, and the wait was met.
		check(
			"normal",
			true,
			true,
			true,
			&[
				(true, false, Some(10), Some(10), false, Finalized),
				(true, false, None, Some(11), false, Finalized), // included, the tracker missed it
				(true, false, Some(10), None, true, InPool),
				(false, true, None, None, false, Refused),
				(true, false, None, None, false, Lost),
				(false, false, None, None, false, Unknown), // no reply
			],
		);
		// Fork-only: the tracker saw these in block 10, which the chain dropped.
		check(
			"fork-only",
			true,
			true,
			true,
			&[
				(true, false, Some(10), None, false, Lost),
				(true, false, Some(10), Some(11), false, Finalized), // moved
				(true, false, Some(10), None, true, InPool),
			],
		);
		// Finality timed out: the walk ran, but the head it needed was not final.
		check(
			"finality-timeout",
			true,
			false,
			true,
			&[
				(true, false, Some(10), None, false, Unverified),
				(true, false, Some(10), Some(9), false, Finalized),
				(true, false, None, None, false, Unknown),
				(true, false, None, None, true, InPool),
			],
		);
		// The walk failed: no finalized evidence, and the pool was not listed.
		check(
			"walk-failed",
			false,
			false,
			false,
			&[
				(true, false, Some(10), None, false, Unverified),
				(false, true, None, None, false, Refused),
				(true, false, None, None, false, Unknown),
			],
		);
	}
}
