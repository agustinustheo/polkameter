//! The loss check, after recovery. Every flood tx we sent gets one [`Status`] from its own
//! evidence, and every printed count is tallied from those statuses, so they partition `sent`.
//! It waits until the last block with our txs is finalized, walks the finalized chain for them,
//! and lets the scenario check chain state for a sample.

use std::{
	collections::{HashMap, HashSet},
	time::Duration,
};

use polkameter_plugin_sdk::chain::{ChainError, Client, tx_hash};
use polkameter_plugin_sdk::observers::summary::{Finality, Loss, LostTx, NodePool, StateSample};
use serde::Serialize;

use crate::engine::measurement::PluginState;
use crate::load::{source::TxHash, tracker::FloodTx};

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
	state: Option<&PluginState>,
	finality_wait_ms: u64,
) -> (Loss, Vec<LostTx>, Vec<serde_json::Value>) {
	let mut notes = Vec::new();
	let mut loss = Loss::default();
	loss.sent = run.flood.len() as u64;
	// Snapshot the pool first, then require finality through the best head observed
	// after listing it. A tx can leave the pool while we walk, but cannot disappear
	// between a late pool snapshot and an earlier chain cutoff. The reads are awaited in
	// this order, never joined concurrently.
	let ready = pending(client).await.map(|hashes| hashes.into_iter().collect::<HashSet<_>>());
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
	// Finalized dispatch evidence settles every tx: the walk ran up to the head it needed.
	let settled = on_chain.is_some() && loss.finalized.as_ref().is_some_and(|f| f.waited_for_it);
	let evidence = Evidence { found, finalized_failed: &finalized_failed, settled };
	let rows = ledger(run.flood, run.included_in, &evidence, ready.as_ref(), cutoff, known);
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
		let c = &mut loss.on_chain;
		c.blocks = (*first, *last);
		c.included = loss.included;
		c.missed = rows.iter().filter(|r| r.finalized.is_some() && r.best.is_none()).count() as u64;
		c.only_on_fork =
			rows.iter().filter(|r| r.finalized.is_none() && r.best.is_some()).count() as u64;
		c.moved = rows
			.iter()
			.filter(|r| matches!((r.best, r.finalized), (Some(b), Some(f)) if b != f))
			.count() as u64;
		c.failed_only_on_fork = rows.iter().filter(|r| r.failed_on_fork).count() as u64;
	}
	loss.node_pool = node_pool.await;
	let at = finalized.as_ref().ok().map(|(_, at)| *at);
	if let Some(check) = state {
		let ok = state_candidates(run.flood, found, &finalized_failed);
		let result =
			state_check(&ok, at, |sample, at| async move { check.check(&sample, at).await }).await;
		if let Err(e) = &result {
			notes.push(format!("state check: {e}"));
		}
		(loss.state, loss.state_error) = match result {
			Ok(sample) => (sample, None),
			Err(e) => (None, Some(e)),
		};
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
				check.check(&[row.hash], at).await.ok().map(|s| s.missing == 0)
			},
			_ => None,
		};
		let validate = match client.validate(&row.tx.bytes, "lost tx").await {
			Ok(()) => "valid".to_owned(),
			Err(e) => e.to_string(),
		};
		lost.push(LostTx {
			hash: polkameter_plugin_sdk::chain::hex0x(row.hash),
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

/// The txs the state check samples: every flood tx in a finalized block whose dispatch did not
/// fail there, in send order. The finalized chain decides, not the tracker's best blocks: a tx
/// that failed on a dropped fork and succeeded in the finalized block is checked too.
fn state_candidates(
	flood: &HashMap<TxHash, FloodTx>,
	found: Option<&HashMap<TxHash, u32>>,
	finalized_failed: &HashSet<TxHash>,
) -> Vec<TxHash> {
	let Some(found) = found else { return Vec::new() };
	let mut ok: Vec<(&TxHash, &FloodTx)> = flood
		.iter()
		.filter(|(hash, _)| found.contains_key(*hash) && !finalized_failed.contains(*hash))
		.collect();
	ok.sort_by_key(|(hash, tx)| (tx.sent_at, **hash));
	ok.into_iter().map(|(hash, _)| *hash).collect()
}

/// The plan's state check over a sample of `ok` (finalized, not failed) at the finalized block
/// `at`. `Ok(None)`: nothing to check (no tx finalized without failing). `Err`: the configured
/// check gave no result (it errored or timed out, or there is no finalized block to read).
async fn state_check<F>(
	ok: &[TxHash],
	at: Option<[u8; 32]>,
	check: impl FnOnce(Vec<TxHash>, [u8; 32]) -> F,
) -> Result<Option<StateSample>, String>
where
	F: std::future::Future<Output = Result<StateSample, polkameter_plugin_sdk::chain::ChainError>>,
{
	let Some(at) = at else { return Err("no finalized block to check state at".into()) };
	let n = STATE_SAMPLE.min(ok.len());
	if n == 0 {
		return Ok(None);
	}
	let sample: Vec<TxHash> = (0..n).map(|i| ok[i * ok.len() / n]).collect();
	check(sample, at).await.map(Some).map_err(|e| e.to_string())
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
	/// Failed after inclusion, by the best evidence there is (see [`dispatch`]).
	failed: bool,
	/// Failed only in a best block the finalized chain dropped.
	failed_on_fork: bool,
	json: serde_json::Value,
}

/// What the finalized chain says, for the ledger.
struct Evidence<'a> {
	/// The finalized walk: the block each of our txs is in (`None` when it did not run).
	found: Option<&'a HashMap<TxHash, u32>>,
	/// Of those, the ones with an `ExtrinsicFailed` event.
	finalized_failed: &'a HashSet<TxHash>,
	/// The walk reached the head the check needed: a tx in no finalized block is in none.
	settled: bool,
}

/// Whether a tx failed after inclusion: (failed, failed only on a fork). The finalized block's
/// events decide when there is one. A best-block failure counts only while finality is not
/// settled; once it is, a failure in a block the finalized chain dropped is a fork-only failure,
/// counted apart, not a failure of the run.
fn dispatch(failed_in_best: bool, failed_finalized: Option<bool>, settled: bool) -> (bool, bool) {
	match failed_finalized {
		Some(failed) => (failed, failed_in_best && !failed),
		None if settled => (false, failed_in_best),
		None => (failed_in_best, false),
	}
}

/// One row per flood tx.
fn ledger<'a>(
	flood: &'a HashMap<TxHash, FloodTx>,
	included_in: &HashMap<TxHash, u32>,
	evidence: &Evidence<'_>,
	ready: Option<&HashSet<TxHash>>,
	cutoff: Option<u32>,
	known: bool,
) -> Vec<Row<'a>> {
	flood
		.iter()
		.map(|(hash, tx)| {
			let best = included_in.get(hash).copied();
			let finalized = evidence.found.and_then(|f| f.get(hash)).copied();
			let ready = ready.map(|pool| pool.contains(hash));
			let status = classify(tx, best, finalized, ready, known);
			let failed_finalized = finalized.map(|_| evidence.finalized_failed.contains(hash));
			let (failed, failed_on_fork) =
				dispatch(tx.failed_in_block, failed_finalized, evidence.settled);
			let json = serde_json::json!({"version":1,"hash":hex::encode(hash),"step":tx.step,
				"sentAt":tx.sent_at,"poolSnapshotBlock":cutoff,"accepted":tx.accepted,
				"replyMs":tx.reply_ms,"inclusionMs":tx.inclusion_ms,"bestBlock":best,
				"failedInBestBlock":tx.failed_in_block,"finalizedBlock":finalized,
				"failedInFinalizedBlock":failed_finalized,"failedOnlyOnFork":failed_on_fork,
				"inReadyPool":ready,"status":status});
			Row { hash: *hash, tx, status, best, finalized, failed, failed_on_fork, json }
		})
		.collect()
}

/// Our flood txs in the finalized blocks `first..=last`, with the block each is in.
async fn finalized_txs(
	client: &Client,
	flood: &HashMap<TxHash, FloodTx>,
	first: u32,
	last: u32,
) -> Result<(HashMap<TxHash, u32>, HashSet<TxHash>), polkameter_plugin_sdk::chain::ChainError> {
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
		let events = polkameter_plugin_sdk::chain::events(&client.at(hash).await?).await?;
		let failed_in_block = polkameter_plugin_sdk::chain::failed_extrinsics(&events);
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
) -> Result<(u32, [u8; 32]), polkameter_plugin_sdk::chain::ChainError> {
	let deadline = tokio::time::Instant::now() + Duration::from_millis(finality_wait_ms);
	loop {
		let at = client.finalized().await?;
		let number = at.block_number() as u32;
		if number >= block || tokio::time::Instant::now() > deadline {
			return Ok((number, at.block_hash().0));
		}
		tokio::time::sleep(Duration::from_secs(2)).await;
	}
}

/// Hashes of the ready txs in the node's pool.
async fn pending(client: &Client) -> Result<Vec<[u8; 32]>, ChainError> {
	let list: Vec<String> =
		client.request("author_pendingExtrinsics", subxt::rpcs::rpc_params![]).await?;
	list.iter()
		.map(|x| {
			hex::decode(x.trim_start_matches("0x"))
				.map(|b| tx_hash(&b))
				.map_err(|e| ChainError::Read { what: "pending hex", detail: e.to_string() })
		})
		.collect()
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
		let failed = HashSet::new();
		let evidence =
			Evidence { found: walked.then_some(&found), finalized_failed: &failed, settled: known };
		let pool = pool_listed.then_some(&ready);
		let rows = ledger(&flood, &included_in, &evidence, pool, Some(12), known);
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

	/// A tx failed in a best block on a fork, then dispatched successfully in the finalized block.
	#[test]
	fn a_fork_failure_does_not_count_once_the_finalized_block_succeeded() {
		let hash = [7; 32];
		let tx = FloodTx { accepted: true, failed_in_block: true, ..Default::default() };
		let flood = HashMap::from([(hash, tx)]);
		let included_in = HashMap::from([(hash, 10)]);
		let found = HashMap::from([(hash, 11)]);
		let none = HashSet::new();
		let evidence = Evidence { found: Some(&found), finalized_failed: &none, settled: true };
		let rows = ledger(&flood, &included_in, &evidence, Some(&HashSet::new()), Some(12), true);
		assert_eq!(rows[0].status, Finalized);
		assert!(!rows[0].failed, "finalized success wins over the fork's failure");
		assert!(rows[0].failed_on_fork, "the fork failure is counted apart");
		assert_eq!(rows[0].json["failedOnlyOnFork"], true);
		// Failed in the finalized block too: a failure, and not a fork-only one.
		let failed = HashSet::from([hash]);
		let evidence = Evidence { found: Some(&found), finalized_failed: &failed, settled: true };
		let rows = ledger(&flood, &included_in, &evidence, Some(&HashSet::new()), Some(12), true);
		assert!(rows[0].failed && !rows[0].failed_on_fork);
	}

	/// A tx failed on a dropped fork, then succeeded in the finalized block: the tracker never
	/// counted it as a success, but the state check must still read it.
	#[test]
	fn state_check_candidates_come_from_the_finalized_chain() {
		let (a, b, c) = ([1; 32], [2; 32], [3; 32]);
		let tx =
			|sent_at, failed_in_block| FloodTx { sent_at, failed_in_block, ..Default::default() };
		let flood = HashMap::from([(a, tx(3, true)), (b, tx(1, false)), (c, tx(2, false))]);
		let found = HashMap::from([(a, 11), (b, 10), (c, 10)]);
		let failed = HashSet::from([c]);
		assert_eq!(state_candidates(&flood, Some(&found), &failed), [b, a]);
		assert!(state_candidates(&flood, None, &failed).is_empty(), "no walk, no candidates");
	}

	#[test]
	fn dispatch_uses_finalized_evidence_when_there_is_some() {
		// (failed in best, failed in finalized, settled) -> (failed, failed only on a fork)
		for (best, finalized, settled, want) in [
			(true, Some(false), true, (false, true)),
			(true, Some(true), true, (true, false)),
			(false, Some(true), true, (true, false)),
			(false, Some(false), true, (false, false)),
			// Settled, in no finalized block: its best block was dropped.
			(true, None, true, (false, true)),
			// Not settled: the best block is the only evidence.
			(true, None, false, (true, false)),
			(false, None, false, (false, false)),
		] {
			assert_eq!(dispatch(best, finalized, settled), want, "{best} {finalized:?} {settled}");
		}
	}

	#[test]
	fn a_state_check_without_a_result_is_an_error_not_a_pass() {
		use polkameter_plugin_sdk::chain::ChainError;
		let ok = [[1; 32], [2; 32]];
		let run = |at, result: Result<StateSample, ChainError>| {
			futures_util::FutureExt::now_or_never(state_check(&ok, at, |sample, _| {
				assert_eq!(sample.len(), 2);
				std::future::ready(result)
			}))
			.expect("ready")
		};
		let timeout = ChainError::Timeout("plugin state check: timed out after 60 s".into());
		assert_eq!(
			run(Some([0; 32]), Err(timeout)).unwrap_err(),
			"plugin state check: timed out after 60 s"
		);
		let sample = StateSample { checked: 2, missing: 0, detail: "ok".into() };
		assert_eq!(run(Some([0; 32]), Ok(sample.clone())), Ok(Some(sample.clone())));
		assert!(run(None, Ok(sample)).unwrap_err().contains("no finalized block"));
		let nothing =
			futures_util::FutureExt::now_or_never(state_check(&[], Some([0; 32]), |_, _| {
				std::future::ready(Err(ChainError::Timeout("never called".into())))
			}));
		assert_eq!(nothing, Some(Ok(None)), "nothing finalized: nothing to check");
	}
}
