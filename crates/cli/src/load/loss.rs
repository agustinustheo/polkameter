//! The loss check, after recovery. Every flood tx we sent gets one [`Status`] from its own
//! evidence, and every printed count is tallied from those statuses, so they partition `sent`.
//! It waits until the last block with our txs is finalized, walks the finalized chain for them,
//! and lets the scenario check chain state for a sample. Its reads go through [`Live`], which
//! opens the connection again when it drops, with the same budget as the load's.

use std::{
	collections::{HashMap, HashSet},
	time::Duration,
};

use polkameter_plugin_sdk::chain::{AtBlock, ChainError, Client, account_at, tx_hash};
use polkameter_plugin_sdk::observers::summary::{Finality, Loss, LostTx, NodePool, StateSample};
use serde::Serialize;
use tokio_util::sync::CancellationToken;

use crate::engine::measurement::PluginState;
use crate::load::{
	reconnect::{Backoff, Reconnect, reconnect},
	source::TxHash,
	tracker::FloodTx,
};

/// The loss check's connection. A read that fails opens the connection again, with the
/// reconnect budget (15 s), and is made once more on the new one: a dropped connection is not
/// the chain's, and a node back within the budget still has its loss verified. Once the budget
/// ran out, reads are not tried again.
pub struct Live<C, F> {
	conn: C,
	connect: F,
	backoff: Backoff,
	gave_up: bool,
}

impl<C: Clone, F, Fut> Live<C, F>
where
	F: FnMut() -> Fut,
	Fut: std::future::Future<Output = Result<C, ChainError>>,
{
	/// Reads on `conn`, and opens a new one with `connect` when a read fails.
	pub fn new(conn: C, connect: F, backoff: Backoff) -> Self {
		Self { conn, connect, backoff, gave_up: false }
	}

	/// The connection now.
	pub fn conn(&self) -> &C {
		&self.conn
	}

	/// `read` on the connection; when it fails, on a new connection.
	pub async fn read<T, R>(&mut self, read: impl Fn(C) -> R) -> Result<T, ChainError>
	where
		R: std::future::Future<Output = Result<T, ChainError>>,
	{
		let error = match read(self.conn.clone()).await {
			Ok(value) => return Ok(value),
			Err(e) if self.gave_up => return Err(e),
			Err(e) => e,
		};
		crate::redact::diag!("loss check: {error}; opening the connection again");
		match reconnect(&mut self.connect, &self.backoff, &CancellationToken::new()).await {
			Reconnect::Connected(conn) => {
				self.conn = conn;
				read(self.conn.clone()).await
			},
			Reconnect::Stopped | Reconnect::GaveUp(_) => {
				self.gave_up = true;
				Err(error)
			},
		}
	}
}

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
	/// The lowest block a flood tx can be in ([`crate::load::tracker::Tracker::walk_from`]).
	pub walk_from: Option<u32>,
}

/// Runs the loss check, and gives the lost txs for lost.jsonl and the ledger rows. `node_pool`
/// reads the node's own mempool and ready counts from `/metrics`; it runs after the finality
/// wait, once our included txs are pruned. Chain errors become notes, never a stop.
#[allow(clippy::too_many_lines)]
pub async fn loss_check<F, Fut>(
	live: &mut Live<Client, F>,
	run: Settled<'_>,
	node_pool: impl std::future::Future<Output = Option<NodePool>>,
	state: Option<&PluginState>,
	finality_wait_ms: u64,
) -> (Loss, Vec<LostTx>, Vec<serde_json::Value>)
where
	F: FnMut() -> Fut,
	Fut: std::future::Future<Output = Result<Client, ChainError>>,
{
	let mut notes = Vec::new();
	let mut loss = Loss::default();
	loss.sent = run.flood.len() as u64;
	// Snapshot the pool first, then require finality through the best head observed
	// after listing it. A tx can leave the pool while we walk, but cannot disappear
	// between a late pool snapshot and an earlier chain cutoff. The reads are awaited in
	// this order, never joined concurrently.
	let ready = live.read(|c: Client| async move { pending(&c).await }).await;
	let ready = ready.map(|hashes| hashes.into_iter().collect::<HashSet<_>>());
	let ready = ready.map_err(|e| notes.push(format!("the pool could not be listed: {e}"))).ok();
	let cutoff = live.read(|c: Client| async move { c.best_number().await }).await;
	let cutoff = cutoff.map(|number| number.max(run.last_ours_block));
	let cutoff = cutoff
		.map_err(|e| notes.push(format!("pool snapshot head unavailable: {e}")))
		.ok();
	let required_block = cutoff.unwrap_or(run.last_ours_block);
	let finalized = wait_finalized(live, required_block, finality_wait_ms).await;
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
			if let Some(first) = run.walk_from {
				match finalized_txs(live, run.flood, first, *number).await {
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
	if run.walk_from.is_none() {
		notes.push("no flood transaction was sent, so the finalized chain was not walked".into());
	}
	let found = on_chain.as_ref().map(|(_, _, found)| found);
	// Absence is useful only after the walk reached the required finalized head. READY
	// says nothing about FUTURE transactions: loss also needs a consumed finalized nonce.
	let settled = on_chain.is_some() && loss.finalized.as_ref().is_some_and(|f| f.waited_for_it);
	let mut nonces = HashMap::new();
	if settled && let Ok((_, at)) = &finalized {
		let candidates: Vec<_> = run
			.flood
			.iter()
			.filter(|(hash, tx)| {
				!found.is_some_and(|f| f.contains_key(*hash))
					&& !ready.as_ref().is_some_and(|r| r.contains(*hash))
					&& !tx.refused && (tx.accepted || run.included_in.contains_key(*hash))
			})
			.map(|(hash, tx)| (*hash, tx.bytes.clone()))
			.collect();
		if !candidates.is_empty() {
			match live
				.read(|c: Client| {
					let candidates = &candidates;
					async move { nonces_at(&c.at(*at).await?, candidates).await }
				})
				.await
			{
				Ok(read) => nonces = read,
				Err(e) => notes.push(format!("finalized account nonces could not be read: {e}")),
			}
		}
	}
	let evidence =
		Evidence { found, finalized_failed: &finalized_failed, settled, nonces: &nonces };
	let rows = ledger(run.flood, run.included_in, &evidence, ready.as_ref(), cutoff);
	let may_finalize = rows
		.iter()
		.filter(|r| r.tx.accepted && matches!(r.status, Status::Unknown | Status::Unverified))
		.count();
	if may_finalize > 0 {
		notes.push(format!("{may_finalize} accepted txs not verified final or lost — may still finalize; see transactions.jsonl for nonce evidence"));
	}
	let count = |status: Status| rows.iter().filter(|r| r.status == status).count() as u64;
	loss.accepted = run.flood.values().filter(|tx| tx.accepted).count() as u64;
	loss.included = count(Status::Finalized);
	loss.refused = count(Status::Refused);
	loss.unverified = count(Status::Unverified);
	loss.unknown = count(Status::Unknown);
	loss.in_pool = ready.is_some().then(|| count(Status::InPool));
	loss.lost = settled.then(|| count(Status::Lost));
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
		// Not tried again: an invalid tx is an answer, not a dropped connection.
		let validate = match live.conn().validate(&row.tx.bytes, "lost tx").await {
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
	/// Absent from the finalized walk, with a finalized signer nonce past its own.
	Lost,
	/// Seen in a best block, but neither finalization nor loss is proven.
	Unverified,
	/// Not enough evidence for another status; an accepted tx may still finalize.
	Unknown,
}

fn classify(
	tx: &FloodTx,
	best: Option<u32>,
	finalized: Option<u32>,
	ready: Option<bool>,
	nonce_consumed: bool,
) -> Status {
	if finalized.is_some() {
		Status::Finalized
	} else if ready == Some(true) {
		Status::InPool
	} else if tx.refused {
		Status::Refused
	} else if nonce_consumed && (tx.accepted || best.is_some()) {
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
	/// Transaction nonce and signer account nonce, read at the walk's finalized upper bound.
	nonces: &'a HashMap<TxHash, (u64, u64)>,
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
) -> Vec<Row<'a>> {
	flood
		.iter()
		.map(|(hash, tx)| {
			let best = included_in.get(hash).copied();
			let finalized = evidence.found.and_then(|f| f.get(hash)).copied();
			let ready = ready.map(|pool| pool.contains(hash));
			let nonces = evidence.nonces.get(hash).copied();
			let consumed = evidence.settled && nonces.is_some_and(|(tx, account)| account > tx);
			let status = classify(tx, best, finalized, ready, consumed);
			let reason = match status {
				Status::Lost => Some(
					"absent from finalized chain; finalized signer nonce has passed transaction nonce",
				),
				Status::Unknown | Status::Unverified if tx.accepted || best.is_some() => {
					Some(if nonces.is_some_and(|(tx, account)| account <= tx) {
						"accepted, not finalized, nonce not yet used — may still finalize"
					} else {
						"finalization or nonce consumption could not be verified — may still finalize"
					})
				},
				_ => None,
			};
			let failed_finalized = finalized.map(|_| evidence.finalized_failed.contains(hash));
			let (failed, failed_on_fork) =
				dispatch(tx.failed_in_block, failed_finalized, evidence.settled);
			// An untimed tx has no inclusion time here either (`inclusionMs` is null).
			let json = serde_json::json!({"version":1,"hash":hex::encode(hash),"step":tx.step,
				"sentAt":tx.sent_at,"poolSnapshotBlock":cutoff,"accepted":tx.accepted,
				"replyMs":tx.reply_ms,"inclusionMs":tx.inclusion_ms,"untimed":tx.untimed,"bestBlock":best,
				"failedInBestBlock":tx.failed_in_block,"finalizedBlock":finalized,
				"failedInFinalizedBlock":failed_finalized,"failedOnlyOnFork":failed_on_fork,
				"nonce":nonces.map(|n| n.0),"finalizedAccountNonce":nonces.map(|n| n.1),
				"inReadyPool":ready,"status":status,"statusReason":reason});
			Row { hash: *hash, tx, status, best, finalized, failed, failed_on_fork, json }
		})
		.collect()
}

/// Decode the signed bytes using runtime metadata, not plugin-specific metadata (which may
/// contain only a signer index). Unsupported formats stay unknown, never guessed from offsets.
async fn nonces_at(
	at: &AtBlock,
	candidates: &[(TxHash, Vec<u8>)],
) -> Result<HashMap<TxHash, (u64, u64)>, ChainError> {
	use subxt::ext::{
		frame_decode::extrinsics::ExtrinsicTypeInfo as _, scale_decode::DecodeAsType,
	};
	#[derive(DecodeAsType)]
	#[decode_as_type(crate_path = "subxt::ext::scale_decode")]
	enum Address {
		Id([u8; 32]),
	}

	let metadata = at.metadata_ref();
	let Ok(signature) = metadata.extrinsic_signature_info() else { return Ok(HashMap::new()) };
	let mut accounts = HashMap::new();
	let mut nonces = HashMap::new();
	for (hash, bytes) in candidates {
		let exts = at.extrinsics().from_bytes(vec![bytes.clone()]).await;
		let Some(Ok(ext)) = exts.iter().next() else { continue };
		let Some(address) = ext.address_bytes() else { continue };
		let account =
			Address::decode_as_type(&mut &*address, signature.address_id, metadata.types())
				.map(|Address::Id(id)| id)
				.or_else(|_| {
					<[u8; 32]>::decode_as_type(
						&mut &*address,
						signature.address_id,
						metadata.types(),
					)
				});
		let Ok(account) = account else { continue };
		let Some(nonce) = ext.transaction_extensions().and_then(|e| e.nonce()) else { continue };
		let finalized = match accounts.get(&account) {
			Some(nonce) => *nonce,
			None => {
				let nonce = account_at(at, account).await?.nonce;
				accounts.insert(account, nonce);
				nonce
			},
		};
		nonces.insert(*hash, (nonce, finalized));
	}
	Ok(nonces)
}

/// Our flood txs in the finalized blocks `first..=last`, with the block each is in.
async fn finalized_txs<F, Fut>(
	live: &mut Live<Client, F>,
	flood: &HashMap<TxHash, FloodTx>,
	first: u32,
	last: u32,
) -> Result<(HashMap<TxHash, u32>, HashSet<TxHash>), ChainError>
where
	F: FnMut() -> Fut,
	Fut: std::future::Future<Output = Result<Client, ChainError>>,
{
	let mut found = HashMap::new();
	let mut failed = HashSet::new();
	for n in first..=last {
		let (hash, body) = live
			.read(|c: Client| async move {
				let hash = c.block_hash(n).await?;
				Ok((hash, c.body(hash).await?))
			})
			.await?;
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
		let events = live
			.read(|c: Client| async move {
				polkameter_plugin_sdk::chain::events(&c.at(hash).await?).await
			})
			.await?;
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

async fn wait_finalized<F, Fut>(
	live: &mut Live<Client, F>,
	block: u32,
	finality_wait_ms: u64,
) -> Result<(u32, [u8; 32]), ChainError>
where
	F: FnMut() -> Fut,
	Fut: std::future::Future<Output = Result<Client, ChainError>>,
{
	let deadline = tokio::time::Instant::now() + Duration::from_millis(finality_wait_ms);
	loop {
		let (number, hash) = live
			.read(|c: Client| async move {
				c.finalized().await.map(|at| (at.block_number() as u32, at.block_hash().0))
			})
			.await?;
		if number >= block || tokio::time::Instant::now() > deadline {
			return Ok((number, hash));
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
	type Case = (bool, bool, Option<u32>, Option<u32>, bool, bool, Status);

	/// Runs the ledger over one scenario and checks each status, and that the statuses partition
	/// the sent txs.
	fn check(name: &str, walked: bool, known: bool, pool_listed: bool, cases: &[Case]) {
		let mut flood = HashMap::new();
		let mut included_in = HashMap::new();
		let mut found = HashMap::new();
		let mut ready = HashSet::new();
		let mut nonces = HashMap::new();
		for (index, &(accepted, refused, best, finalized, in_pool, consumed, _)) in
			cases.iter().enumerate()
		{
			let hash = [index as u8; 32];
			let tx = FloodTx { sent_at: index as u64, accepted, refused, ..Default::default() };
			flood.insert(hash, tx);
			if consumed {
				nonces.insert(hash, (7, 8));
			}
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
		let evidence = Evidence {
			found: walked.then_some(&found),
			finalized_failed: &failed,
			settled: known,
			nonces: &nonces,
		};
		let pool = pool_listed.then_some(&ready);
		let rows = ledger(&flood, &included_in, &evidence, pool, Some(12));
		for row in &rows {
			let want = cases[row.tx.sent_at as usize].6;
			assert_eq!(row.status, want, "{name}: tx {}", row.tx.sent_at);
		}
		let total: u64 = [Finalized, InPool, Refused, Lost, Unverified, Unknown]
			.iter()
			.map(|s| rows.iter().filter(|r| r.status == *s).count() as u64)
			.sum();
		assert_eq!(total, flood.len() as u64, "{name}: statuses partition sent");
	}

	/// Acceptance and absence from READY do not rule out waiting behind a nonce gap.
	#[test]
	fn an_accepted_transaction_absent_from_ready_may_still_finalize() {
		check(
			"future pool or not observable",
			true,
			true,
			true,
			&[(true, false, None, None, false, false, Unknown)],
		);
	}

	#[tokio::test]
	#[ignore = "needs a dev chain: scripts/dev-chain.sh start"]
	async fn signed_bytes_supply_nonce_evidence() {
		use polkameter_plugin_sdk::chain::{Call, TxParams, account_id, keypair};
		let target = std::env::var("POLKAMETER_DEV_CHAIN_WS")
			.unwrap_or_else(|_| "ws://127.0.0.1:9944".into());
		let client = Client::connect(&target).await.unwrap();
		let key = keypair("//Alice").unwrap();
		let at = client.finalized().await.unwrap();
		let nonce = account_at(&at, account_id(&key)).await.unwrap().nonce;
		let builder = client.tx_builder().await.unwrap();
		let bytes = builder
			.sign(
				&Call::new(
					"System",
					"remark",
					vec![subxt::dynamic::Value::from_bytes(b"nonce evidence")],
				),
				&key,
				&TxParams::nonce(nonce + 1),
			)
			.unwrap();
		let hash = tx_hash(&bytes);
		let evidence = nonces_at(&at, &[(hash, bytes), ([0; 32], vec![0xff])]).await.unwrap();
		assert_eq!(evidence.get(&hash), Some(&(nonce + 1, nonce)));
		assert!(!evidence.contains_key(&[0; 32]), "undecodable bytes supply no evidence");
	}

	#[test]
	fn loss_requires_a_consumed_finalized_nonce() {
		let hash = [7; 32];
		let flood = HashMap::from([(hash, FloodTx { accepted: true, ..Default::default() })]);
		let found = HashMap::new();
		let failed = HashSet::new();
		let ready = HashSet::new();
		// Same signed tx (nonce 7): a gap, exactly next, consumed, or unreadable state.
		for (nonce, want) in
			[(Some(6), Unknown), (Some(7), Unknown), (Some(8), Lost), (None, Unknown)]
		{
			let nonces = nonce.map(|n| HashMap::from([(hash, (7, n))])).unwrap_or_default();
			let evidence = Evidence {
				found: Some(&found),
				finalized_failed: &failed,
				settled: true,
				nonces: &nonces,
			};
			let rows = ledger(&flood, &HashMap::new(), &evidence, Some(&ready), Some(12));
			assert_eq!(rows[0].status, want, "finalized account nonce {nonce:?}");
			if matches!(nonce, Some(6 | 7)) {
				assert!(
					rows[0].json["statusReason"].as_str().unwrap().contains("nonce not yet used")
				);
			}
			// A missing pool read cannot undo positive nonce evidence.
			assert_eq!(ledger(&flood, &HashMap::new(), &evidence, None, Some(12))[0].status, want);
			// Without a completed finalized walk, a consumed nonce cannot establish absence.
			let evidence = Evidence { found: None, settled: false, ..evidence };
			assert_eq!(
				ledger(&flood, &HashMap::new(), &evidence, Some(&ready), Some(12))[0].status,
				Unknown
			);
			assert_eq!(
				ledger(&flood, &HashMap::from([(hash, 10)]), &evidence, Some(&ready), Some(12))[0]
					.status,
				Unverified
			);
		}
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
				(true, false, Some(10), Some(10), false, false, Finalized),
				(true, false, None, Some(11), false, false, Finalized), // included, the tracker missed it
				(true, false, Some(10), None, true, false, InPool),
				(false, true, None, None, false, false, Refused),
				(true, false, None, None, false, true, Lost),
				(false, false, None, None, false, false, Unknown), // no reply
			],
		);
		// Fork-only: the tracker saw these in block 10, which the chain dropped.
		check(
			"fork-only",
			true,
			true,
			true,
			&[
				(true, false, Some(10), None, false, true, Lost),
				(true, false, Some(10), Some(11), false, false, Finalized), // moved
				(true, false, Some(10), None, true, false, InPool),
			],
		);
		// Finality timed out: the walk ran, but the head it needed was not final.
		check(
			"finality-timeout",
			true,
			false,
			true,
			&[
				(true, false, Some(10), None, false, false, Unverified),
				(true, false, Some(10), Some(9), false, false, Finalized),
				(true, false, None, None, false, false, Unknown),
				(true, false, None, None, true, false, InPool),
			],
		);
		// The walk failed: no finalized evidence, and the pool was not listed.
		check(
			"walk-failed",
			false,
			false,
			false,
			&[
				(true, false, Some(10), None, false, false, Unverified),
				(false, true, None, None, false, false, Refused),
				(true, false, None, None, false, false, Unknown),
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
		let evidence = Evidence {
			found: Some(&found),
			finalized_failed: &none,
			settled: true,
			nonces: &HashMap::new(),
		};
		let rows = ledger(&flood, &included_in, &evidence, Some(&HashSet::new()), Some(12));
		assert_eq!(rows[0].status, Finalized);
		assert!(!rows[0].failed, "finalized success wins over the fork's failure");
		assert!(rows[0].failed_on_fork, "the fork failure is counted apart");
		assert_eq!(rows[0].json["failedOnlyOnFork"], true);
		// Failed in the finalized block too: a failure, and not a fork-only one.
		let failed = HashSet::from([hash]);
		let evidence = Evidence {
			found: Some(&found),
			finalized_failed: &failed,
			settled: true,
			nonces: &HashMap::new(),
		};
		let rows = ledger(&flood, &included_in, &evidence, Some(&HashSet::new()), Some(12));
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
