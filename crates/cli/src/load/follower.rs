//! Follows the chain's best and finalized blocks. Two tasks:
//! - heads: sends [`BlockEvent::Head`] the moment a new best block arrives (the stall rule reads
//!   it), and queues the block for the fetcher, after the blocks it builds on that were never
//!   read: after a reorg the node announces only the new tip, so the branch below it is
//!   filled in;
//! - fetcher: reads each queued block (body, weight, timestamp, events) strictly in arrival
//!   order and sends [`BlockEvent::Fetched`]. In order, so a source never sees a later block
//!   before the inclusions of an earlier one.
//!
//! When the connection drops, the heads task sends [`BlockEvent::Disconnected`], opens it again
//! with backoff, subscribes, and only then sends [`BlockEvent::Reconnected`]: the time between is
//! an observation gap, not a chain stall, and the first block after it has no gap to the block
//! before. The blocks missed meanwhile are filled in below the next best block. A node that stays
//! unreachable for the backoff (about 15 s), or that drops the connection again and again, is
//! [`BlockEvent::Unreachable`]: node down. A reconnect that lands on another node, or on the same
//! node restarted, is [`BlockEvent::OtherNode`]: a warning only. Behind a load balancer a
//! reconnect can reach another healthy node; a chain that is really down still trips the stall
//! and finality stall rules.
//!
//! Every read the heads task makes has [`REQUEST`] to answer and gives way to `stop`, so a node
//! that never answers cannot hold the task past the end of the run.

use std::{collections::HashSet, future::Future, time::Duration};

use subxt::{PolkadotConfig, client::Blocks};
use subxt_rpcs::rpc_params;

use polkameter_plugin_sdk::chain::{
	ChainError, Client, DecodeAsType, Fault, events, failed_extrinsics, fetch, hex0x, tx_hash,
};
use polkameter_plugin_sdk::observers::files::{BlockRecord, Millis};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;

use crate::load::{
	now_ms,
	reconnect::{Backoff, Drops, Reconnect, reconnect},
	source::TxHash,
};

/// What the follower saw.
#[derive(Debug)]
pub enum BlockEvent {
	/// A new best block arrived; not read yet.
	Head {
		/// Number.
		number: u32,
		/// When it arrived.
		seen_at: Millis,
	},
	/// A best block or a filled-in one, read. `record.ours` is for the tracker to fill in.
	Fetched {
		/// The block.
		record: BlockRecord,
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
	/// The follower lost the node (the connection dropped or the subscription ended) and is
	/// reconnecting: no block is observed until [`BlockEvent::Reconnected`]. Not a chain stall.
	Disconnected {
		/// When.
		at: Millis,
		/// Why.
		reason: String,
	},
	/// The follower follows the node again.
	Reconnected {
		/// When.
		at: Millis,
	},
	/// The node stayed unreachable for the whole reconnect backoff, or kept dropping the
	/// connection: the node is down.
	Unreachable(String),
	/// After a reconnect the node is another process (a new peer ID or starting block): a load
	/// balancer sent the connection to another node, or the node restarted. A warning, not node
	/// down: the stall rules judge whether the chain still produces blocks.
	OtherNode(String),
	/// An answer did not decode as our types: an error of our tools, the run stops.
	Tool(String),
}

#[derive(DecodeAsType, Default)]
#[decode_as_type(crate_path = "polkameter_plugin_sdk::chain::scale_decode_reexport")]
struct Weight {
	ref_time: u64,
	proof_size: u64,
}

#[derive(DecodeAsType, Default)]
#[decode_as_type(crate_path = "polkameter_plugin_sdk::chain::scale_decode_reexport")]
struct PerClass {
	normal: Weight,
}

/// The running runtime's normal class limit (ref time, proof size), from `System.BlockWeights`.
pub type Limit = (u64, u64);

/// A block for the fetcher: hash, number, when it arrived, filled in, and the first after the
/// follower reconnected (its gap to the block before is not known).
type Queued = ([u8; 32], u32, Millis, bool, bool);

/// Blocks filled in below one best block at most; a longer walk is an unreadable stretch.
const MAX_FILL: usize = 64;

/// Best-block read errors in a row that count as a lost connection.
const ERRORS_IN_A_ROW: u32 = 3;

/// The longest one read of the heads task may take, as one reconnect attempt.
pub const REQUEST: Duration = Duration::from_secs(5);

/// `read`, failed as a timeout if the node does not answer within [`REQUEST`].
async fn bounded<T>(
	what: &str,
	read: impl Future<Output = Result<T, ChainError>>,
) -> Result<T, ChainError> {
	tokio::time::timeout(REQUEST, read).await.unwrap_or_else(|_| {
		Err(ChainError::Timeout(format!("{what}: no answer in {} s", REQUEST.as_secs())))
	})
}

/// `work`, unless `stop` is cancelled first.
async fn until_stopped<T>(stop: &CancellationToken, work: impl Future<Output = T>) -> Option<T> {
	tokio::select! {
		() = stop.cancelled() => None,
		out = work => Some(out),
	}
}

/// Starts both tasks; they end at `stop`. `url` opens the connection again when it drops.
pub fn start(
	client: Client,
	url: String,
	limit: Limit,
	out: mpsc::UnboundedSender<BlockEvent>,
	stop: CancellationToken,
) {
	let (queue_tx, queue) = mpsc::unbounded_channel();
	let (clients, current) = watch::channel(client.clone());
	tokio::spawn(heads(client, url, clients, out.clone(), queue_tx, stop.clone()));
	tokio::spawn(fetcher(current, limit, out, queue, stop));
}

/// Who the node is, to see that a reconnect reached another process: its peer ID, and the block
/// it started syncing at (`system_syncState.startingBlock`, fixed for a process).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Identity {
	/// `system_localPeerId`.
	pub peer_id: Option<String>,
	/// `system_syncState.startingBlock`.
	pub starting_block: Option<u64>,
}

impl Identity {
	async fn read(client: &Client) -> Self {
		let peer_id = bounded(
			"system_localPeerId",
			client.request::<String>("system_localPeerId", rpc_params![]),
		);
		let sync = bounded(
			"system_syncState",
			client.request::<serde_json::Value>("system_syncState", rpc_params![]),
		);
		let (peer_id, sync) = tokio::join!(peer_id, sync);
		Self {
			peer_id: peer_id.ok(),
			starting_block: sync.ok().and_then(|s| s["startingBlock"].as_u64()),
		}
	}

	/// Why `now` is another process than `self`, if it is: a load balancer sent the
	/// connection to another node, or the node restarted. A value either side could not read
	/// tells nothing.
	pub fn other_node(&self, now: &Identity) -> Option<String> {
		const WHY: &str = "reconnected to a different node (load balancer, or the node restarted)";
		if let (Some(a), Some(b)) = (&self.peer_id, &now.peer_id)
			&& a != b
		{
			return Some(format!("{WHY}: its peer ID changed from {a} to {b}"));
		}
		if let (Some(a), Some(b)) = (self.starting_block, now.starting_block)
			&& a != b
		{
			return Some(format!("{WHY}: its sync starting block changed from {a} to {b}"));
		}
		None
	}

	/// Takes what `now` could read: the next reconnect compares with the node last followed.
	pub fn update(&mut self, now: Identity) {
		self.peer_id = now.peer_id.or(self.peer_id.take());
		self.starting_block = now.starting_block.or(self.starting_block);
	}
}

/// One connection, subscribed to best and finalized blocks.
struct Session {
	client: Client,
	best: Blocks<PolkadotConfig>,
	finalized: Blocks<PolkadotConfig>,
	identity: Identity,
}

impl Session {
	/// Subscribes on `client`; only a session that got this far counts as connected.
	async fn subscribe(client: Client) -> Result<Self, String> {
		let best = client.api().stream_best_blocks().await;
		let finalized = client.api().stream_blocks().await;
		let (best, finalized) = match (best, finalized) {
			(Ok(best), Ok(finalized)) => (best, finalized),
			(Err(e), _) | (_, Err(e)) => return Err(format!("cannot subscribe to blocks: {e}")),
		};
		let identity = Identity::read(&client).await;
		Ok(Self { client, best, finalized, identity })
	}

	/// Connects to `url` and subscribes.
	async fn open(url: &str) -> Result<Self, String> {
		let client = Client::connect(url).await.map_err(|e| e.to_string())?;
		Self::subscribe(client).await
	}
}

/// Follows best and finalized blocks; when the connection drops, reports it, opens it again
/// (connected means subscribed) and follows on. Blocks below the first new best block after a
/// reconnect are filled in, as after a reorg. A node that cannot be reached within the backoff,
/// or that keeps dropping the connection, is down; one that came back as another process is a
/// warning.
async fn heads(
	client: Client,
	url: String,
	clients: watch::Sender<Client>,
	out: mpsc::UnboundedSender<BlockEvent>,
	queue: mpsc::UnboundedSender<Queued>,
	stop: CancellationToken,
) {
	let backoff = Backoff::default();
	let mut followed = Followed::default();
	let mut drops = Drops::default();
	let first = tokio::time::timeout(backoff.attempt, Session::subscribe(client));
	let mut session = match until_stopped(&stop, first).await {
		None => return,
		Some(Ok(session)) => session,
		Some(Err(_)) => Err("subscribing got no answer in time".into()),
	};
	loop {
		let reason = match session {
			Ok(s) => {
				followed.identity.get_or_insert_with(|| s.identity.clone());
				let _ = clients.send(s.client.clone());
				match follow(s, &out, &queue, &stop, &mut followed).await {
					None => return,
					Some(reason) => reason,
				}
			},
			Err(reason) => reason,
		};
		let lost_at = now_ms();
		crate::redact::diag!("blocks: lost the node ({reason}); reconnecting");
		let _ = out.send(BlockEvent::Disconnected { at: lost_at, reason: reason.clone() });
		if !drops.record() {
			let _ = out.send(BlockEvent::Unreachable(format!("blocks: {}", Drops::reason())));
			return;
		}
		let s = match reconnect(|| Session::open(&url), &backoff, &stop).await {
			Reconnect::Connected(s) => s,
			Reconnect::Stopped => return,
			Reconnect::GaveUp(why) => {
				let _ = out.send(BlockEvent::Unreachable(format!(
					"the block follower lost the node ({reason}) and it was {why}"
				)));
				return;
			},
		};
		if let Some(id) = followed.identity.as_mut() {
			if let Some(why) = id.other_node(&s.identity) {
				crate::redact::diag!("blocks: {why}");
				let _ = out.send(BlockEvent::OtherNode(why));
			}
			id.update(s.identity.clone());
		}
		let at = now_ms();
		crate::redact::diag!(
			"blocks: reconnected after {:.1} s",
			at.saturating_sub(lost_at) as f64 / 1000.0
		);
		let _ = out.send(BlockEvent::Reconnected { at });
		followed.after_gap = true;
		session = Ok(s);
	}
}

/// What the follower keeps across reconnects.
#[derive(Default)]
struct Followed {
	/// Blocks queued.
	seen: HashSet<[u8; 32]>,
	/// The first best block followed.
	first: Option<u32>,
	/// The next block queued is the first after a reconnect.
	after_gap: bool,
	/// Who the node was on the last connection.
	identity: Option<Identity>,
}

/// Follows on one session. `None` at `stop`; else why the connection was lost.
async fn follow(
	session: Session,
	out: &mpsc::UnboundedSender<BlockEvent>,
	queue: &mpsc::UnboundedSender<Queued>,
	stop: &CancellationToken,
	f: &mut Followed,
) -> Option<String> {
	let Session { client, mut best, mut finalized, .. } = session;
	// At start the stream goes back to the finalized block; record from the best one on, or
	// the first probes would be sent on old blocks and land "fast".
	let first = match f.first {
		Some(first) => first,
		None => match until_stopped(stop, bounded("best number", client.best_number())).await? {
			Ok(n) => *f.first.insert(n),
			Err(e) => return Some(format!("best number: {e}")),
		},
	};
	let mut errors = 0;
	loop {
		tokio::select! {
			() = stop.cancelled() => return None,
			block = finalized.next() => match block {
				Some(Ok(b)) => { let _ = out.send(BlockEvent::Finalized { number: b.number() as u32, seen_at: now_ms() }); }
				Some(Err(e)) => { let _ = out.send(BlockEvent::Unreadable(format!("finalized: {e}"))); }
				None => return Some("the finalized block stream ended".into()),
			},
			block = best.next() => match block {
				None => return Some("the best block stream ended".into()),
				Some(Err(e)) => {
					errors += 1;
					if errors >= ERRORS_IN_A_ROW {
						return Some(format!("{errors} best block errors in a row, the last: {e}"));
					}
					let _ = out.send(BlockEvent::Unreadable(format!("best: {e}")));
				},
				Some(Ok(b)) => {
					errors = 0;
					if b.number() as u32 >= first && !f.seen.contains(&b.hash().0) {
						let (number, hash, seen_at) = (b.number() as u32, b.hash().0, now_ms());
						let _ = out.send(BlockEvent::Head { number, seen_at });
						let after_gap = std::mem::take(&mut f.after_gap);
						let walk = fill_in(b.header().parent_hash.0, number, first, &f.seen, |h| bounded("parent header", client.parent(h)));
						match until_stopped(stop, walk).await? {
							Ok(missing) => for (h, n) in missing {
								f.seen.insert(h);
								let _ = queue.send((h, n, seen_at, true, false));
							},
							Err(e) => { let _ = out.send(BlockEvent::Unreadable(format!("blocks below {number}: {e}"))); }
						}
						f.seen.insert(hash);
						let _ = queue.send((hash, number, seen_at, false, after_gap));
					}
				},
			},
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

/// Reads of one block that fail on the chain's side, each on the current connection, before it
/// counts as unreadable: a read in flight when the connection dropped is read again on the new
/// one.
const READ_ATTEMPTS: u32 = 4;

async fn fetcher(
	clients: watch::Receiver<Client>,
	limit: Limit,
	out: mpsc::UnboundedSender<BlockEvent>,
	mut queue: mpsc::UnboundedReceiver<Queued>,
	stop: CancellationToken,
) {
	use futures_util::StreamExt;
	let blocks = futures_util::stream::poll_fn(move |cx| queue.poll_recv(cx));
	let mut reads = blocks
		.map(|(hash, number, seen_at, filled_in, after_gap)| {
			let clients = clients.clone();
			async move {
				let mut attempt = 1;
				let result = loop {
					let client = clients.borrow().clone();
					match read(&client, hash, number, seen_at, limit).await {
						Err(e) if e.fault() != Fault::Tool && attempt < READ_ATTEMPTS => {
							attempt += 1;
							tokio::time::sleep(Duration::from_secs(1)).await;
						},
						result => break result,
					}
				};
				(number, seen_at, filled_in, after_gap, result)
			}
		})
		.buffered(FETCH_AHEAD);
	let mut last: Option<Millis> = None; // when the last best block arrived
	loop {
		let (number, seen_at, filled_in, after_gap, result) = tokio::select! {
			() = stop.cancelled() => return,
			Some(r) = reads.next() => r,
		};
		if after_gap {
			last = None; // the follower was disconnected: the gap is not known
		}
		match result {
			Ok((mut record, txs)) => {
				record.fetch_lag_ms = Some(now_ms().saturating_sub(seen_at));
				record.filled_in = filled_in;
				if !filled_in {
					record.gap_ms = last.map(|s| seen_at.saturating_sub(s));
					last = Some(seen_at);
				}
				let _ = out.send(BlockEvent::Fetched { record, txs });
			},
			Err(e) if e.fault() == Fault::Tool => {
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
) -> Result<(BlockRecord, Vec<(TxHash, bool)>), ChainError> {
	let at = client.at(hash).await?;
	let (body, weight, now, events) = tokio::join!(
		client.body(hash),
		fetch::<(), PerClass>(&at, "System", "BlockWeight", ()),
		fetch::<(), u64>(&at, "Timestamp", "Now", ()),
		events(&at),
	);
	let (body, timestamp) = (body?, now?.unwrap_or(0));
	let w = weight?.unwrap_or_default();
	let failed = failed_extrinsics(&events?);
	let pct = |v: u64, max: u64| if max == 0 { 0.0 } else { 100.0 * v as f64 / max as f64 };
	let record = BlockRecord {
		number,
		hash: hex0x(hash),
		seen_at,
		timestamp,
		extrinsics: body.len() as u32,
		normal_ref_time_pct: pct(w.normal.ref_time, limit.0),
		normal_proof_pct: pct(w.normal.proof_size, limit.1),
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

	/// Runs `fill_in` over a chain given as child -> parent.
	fn fill(
		chain: &HashMap<[u8; 32], [u8; 32]>,
		parent: [u8; 32],
		number: u32,
		first: u32,
		seen: &[[u8; 32]],
	) -> Vec<([u8; 32], u32)> {
		let seen: HashSet<[u8; 32]> = seen.iter().copied().collect();
		futures_util::FutureExt::now_or_never(fill_in(parent, number, first, &seen, |x| {
			std::future::ready(
				chain
					.get(&x)
					.copied()
					.ok_or(ChainError::Read { what: "test", detail: "no parent".into() }),
			)
		}))
		.expect("ready")
		.expect("walks")
	}

	#[test]
	fn a_reorg_fills_in_the_new_branch() {
		// Read: 10 -> 11 -> 12. The node switches to 13' on 12' on 11' on 10, and announces 13'.
		let chain = HashMap::from([(h(111), h(10)), (h(112), h(111))]);
		let out = fill(&chain, h(112), 13, 5, &[h(10), h(11), h(12)]);
		assert_eq!(out, [(h(111), 11), (h(112), 12)]);
	}

	#[test]
	fn the_walk_stops_at_the_first_block_followed() {
		// The first best block followed is 5: nothing below it is read, its parent included.
		assert!(fill(&HashMap::new(), h(4), 5, 5, &[]).is_empty());
		// A best block on the last one already read fills in nothing.
		assert!(fill(&HashMap::new(), h(12), 13, 5, &[h(12)]).is_empty());
		let chain = HashMap::from([(h(6), h(5))]);
		assert_eq!(fill(&chain, h(6), 7, 5, &[]), [(h(5), 5), (h(6), 6)]);
	}

	/// Behind a load balancer a reconnect can reach another healthy node: a new peer ID or sync
	/// starting block is a warning that names both causes, not a restart.
	#[test]
	fn a_new_peer_id_or_starting_block_is_another_node() {
		let id = |peer: Option<&str>, start| Identity {
			peer_id: peer.map(str::to_owned),
			starting_block: start,
		};
		let before = id(Some("12D3a"), Some(100));
		assert_eq!(before.other_node(&id(Some("12D3a"), Some(100))), None);
		let why = before.other_node(&id(Some("12D3b"), Some(100))).unwrap();
		assert!(
			why.starts_with(
				"reconnected to a different node (load balancer, or the node restarted)"
			),
			"{why}"
		);
		assert!(why.contains("peer ID changed from 12D3a to 12D3b"), "{why}");
		// The node key is kept on disk: the same peer ID, but it started again.
		let why = before.other_node(&id(Some("12D3a"), Some(140))).unwrap();
		assert!(why.contains("sync starting block changed from 100 to 140"), "{why}");
		// What could not be read tells nothing.
		assert_eq!(before.other_node(&id(None, None)), None);
		// The next reconnect compares with the node last followed, keeping what it could not read.
		let mut last = before.clone();
		last.update(id(Some("12D3b"), None));
		assert_eq!(last, id(Some("12D3b"), Some(100)));
		assert_eq!(last.other_node(&id(Some("12D3b"), Some(100))), None);
	}

	/// A read the node never answers fails after [`REQUEST`], and `stop` ends a walk that is still
	/// waiting: neither holds the heads task past the run.
	#[tokio::test(start_paused = true)]
	async fn a_read_that_never_answers_times_out_and_stop_ends_it() {
		let started = tokio::time::Instant::now();
		let never = std::future::pending::<Result<u32, ChainError>>();
		let err = bounded("best number", never).await.unwrap_err();
		assert!(err.to_string().contains("best number: no answer in 5 s"), "{err}");
		assert_eq!(started.elapsed(), REQUEST);

		// A fill-in walk whose parent reads never answer ends with an error, not never.
		let seen = HashSet::new();
		let walk = fill_in(h(9), 10, 1, &seen, |_| {
			bounded("parent header", std::future::pending::<Result<[u8; 32], ChainError>>())
		});
		assert!(walk.await.unwrap_err().to_string().contains("parent header"));

		let stop = CancellationToken::new();
		let cancel = stop.clone();
		tokio::spawn(async move {
			tokio::time::sleep(Duration::from_millis(300)).await;
			cancel.cancel();
		});
		let started = tokio::time::Instant::now();
		let walk = fill_in(h(9), 10, 1, &seen, |_| std::future::pending());
		assert!(until_stopped(&stop, walk).await.is_none());
		assert_eq!(started.elapsed(), Duration::from_millis(300));
	}
}
