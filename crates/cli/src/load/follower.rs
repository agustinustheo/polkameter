//! Follows the chain's best and finalized blocks. Two tasks:
//! - heads: sends [`BlockEvent::Head`] the moment a new best block arrives (the stall rule reads
//!   it), and queues the block for the fetcher, after the blocks it builds on that were never
//!   read: after a reorg the node announces only the new tip, so the branch below it is
//!   filled in;
//! - fetcher: reads each queued block (body, weight, timestamp, events) strictly in arrival
//!   order and sends [`BlockEvent::Fetched`]. In order, so a source never sees a later block
//!   before the inclusions of an earlier one. A block's gap is the chain's own time per block,
//!   from its `Timestamp.Now`, since the highest block read before it: when PolkaMeter received
//!   either, and whatever delayed that, plays no part.
//!
//! When the connection drops, the heads task sends [`BlockEvent::Disconnected`], opens it again
//! with backoff, subscribes, and only then sends [`BlockEvent::Reconnected`]: the time between is
//! an observation gap, not a chain stall. The blocks missed meanwhile are filled in below the
//! next best block. A connection that stays open but on which the node answers nothing, neither
//! a block nor the best block asked for every [`REQUEST`], for [`SILENT`], is lost the same way,
//! from its last answer: a node that stops answering is unreachable, not a chain that stalled.
//! Nothing answered for the reconnect budget (15 s from the last answer) is
//! [`BlockEvent::Unreachable`], once; the follower keeps trying, so a node that comes back
//! restarted is still seen. A connection that drops again and again is [`BlockEvent::Unstable`]:
//! the follower goes on, and the load ends without blaming the chain.
//!
//! After a reconnect the node's own identity tells what changed ([`Identity`]): the same peer ID
//! with a new sync starting block is the same node restarted, [`BlockEvent::Restarted`]: node
//! down. A new peer ID is another node (a load balancer), [`BlockEvent::OtherNode`]: a warning
//! only; a chain that is really down still trips the stall and finality stall rules. Neither
//! changed is the connection only (a proxy restarted, the network dropped): nothing to report.
//! The process behind the endpoint's port is no evidence, because it may be a proxy.
//!
//! Every read the heads task makes has [`REQUEST`] to answer and gives way to `stop`, so a node
//! that never answers cannot hold the task past the end of the run.

use std::{collections::HashSet, future::Future, time::Duration};

use subxt::rpcs::rpc_params;
use subxt::{PolkadotConfig, client::Blocks};

use polkameter_plugin_sdk::chain::{
	ChainError, Client, DecodeAsType, Fault, decode_err, events, failed_extrinsics, fetch, hex0x,
	tx_hash,
};
use polkameter_plugin_sdk::observers::{BlockRecord, Millis};
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
	/// The node, or the network between, answered nothing for the reconnect budget: not node
	/// down by itself, since the follower can't tell the two apart.
	Unreachable(String),
	/// After a reconnect the node is the same node, restarted: node down.
	Restarted(String),
	/// After a reconnect the node is another node (a new peer ID), or what changed is not known:
	/// a load balancer sent the connection to another node. A warning, not node down: the stall
	/// rules judge whether the chain still produces blocks.
	OtherNode(String),
	/// The connection dropped more than [`crate::load::reconnect::MAX_DROPS`] times in
	/// [`crate::load::reconnect::DROP_WINDOW`], and the node did not restart: the connection is
	/// unstable. Sent once; the follower goes on.
	Unstable(String),
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

/// A block for the fetcher: hash, number, when it arrived, and whether it is filled in.
type Queued = ([u8; 32], u32, Millis, bool);

/// Blocks filled in below one best block at most; a longer walk is an unreadable stretch.
const MAX_FILL: usize = 64;

/// Best-block read errors in a row that count as a lost connection.
const ERRORS_IN_A_ROW: u32 = 3;

/// The longest one read of the heads task may take, as one reconnect attempt; also how often the
/// follower asks the node for its best block.
pub const REQUEST: Duration = Duration::from_secs(5);

/// A connection on which the node answered nothing for this long is lost.
pub const SILENT: Duration = Duration::from_secs(10);

/// When the node last answered anything on the follower's connection: a block, or its best block
/// asked for.
#[derive(Debug, Clone, Copy)]
struct Liveness {
	at: tokio::time::Instant,
	wall: Millis,
}

impl Liveness {
	/// The node answers now.
	fn new() -> Self {
		Self { at: tokio::time::Instant::now(), wall: now_ms() }
	}

	/// The node answered.
	fn answered(&mut self) {
		*self = Self::new();
	}

	/// When the node last answered, if it has answered nothing for [`SILENT`] since: the
	/// connection is lost from then.
	fn silent_since(&self) -> Option<Millis> {
		(self.at.elapsed() >= SILENT).then_some(self.wall)
	}

	/// What is left of `budget` (the reconnect budget) after the time with no answer.
	fn left_of(&self, budget: &Backoff) -> Backoff {
		Backoff { total: budget.total.saturating_sub(self.at.elapsed()), ..budget.clone() }
	}
}

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

/// The running runtime's normal class limit (ref time, proof size): `System.BlockWeights`.
/// Read it here, never from the runtime source: live previewnet allows 1.5 s, `main` 1.7 s.
pub async fn normal_limit(client: &Client) -> Result<(u64, u64), ChainError> {
	// `Weight { ref_time, proof_size }` decodes as the tuple `(ref_time, proof_size)`.
	#[derive(DecodeAsType)]
	#[decode_as_type(crate_path = "polkameter_plugin_sdk::chain::scale_decode_reexport")]
	struct Class {
		max_total: Option<(u64, u64)>,
	}
	#[derive(DecodeAsType)]
	#[decode_as_type(crate_path = "polkameter_plugin_sdk::chain::scale_decode_reexport")]
	struct PerClass {
		normal: Class,
	}
	#[derive(DecodeAsType)]
	#[decode_as_type(crate_path = "polkameter_plugin_sdk::chain::scale_decode_reexport")]
	struct BlockWeights {
		per_class: PerClass,
	}
	let at = client.finalized().await?;
	let w = at
		.constants()
		.entry(subxt::dynamic::constant::<BlockWeights>("System", "BlockWeights"))
		.map_err(decode_err("System.BlockWeights"))?;
	Ok(w.per_class.normal.max_total.unwrap_or_default())
}

/// The parent hash of block `hash`.
async fn parent(client: &Client, hash: [u8; 32]) -> Result<[u8; 32], ChainError> {
	let mut header: serde_json::Value =
		client.request("chain_getHeader", rpc_params![hex0x(hash)]).await?;
	serde_json::from_value::<subxt::utils::H256>(header["parentHash"].take())
		.map(|h| h.0)
		.map_err(|e| ChainError::Read {
			what: "header",
			detail: format!("no parent hash for {}: {e}", hex0x(hash)),
		})
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

	/// Why `now` is the node of `self` restarted, if it is: the same peer ID, and a new sync
	/// starting block (the best block when the process started). A proxy or a network between
	/// changes neither, and another node has another peer ID.
	pub fn restarted(&self, now: &Identity) -> Option<String> {
		let (Some(a), Some(b)) = (&self.peer_id, &now.peer_id) else { return None };
		let (Some(from), Some(to)) = (self.starting_block, now.starting_block) else { return None };
		(a == b && from != to).then(|| {
			format!(
				"the node restarted: after a reconnect it has the same peer ID, {b}, and its sync starting block changed from {from} to {to}"
			)
		})
	}

	/// Why `now` is another node than `self`, if it is: its peer ID changed (a load balancer
	/// sent the connection to another node, or a node restarted with a new key), or, with a
	/// peer ID either side could not read, its sync starting block changed. A value either side
	/// could not read tells nothing.
	pub fn other_node(&self, now: &Identity) -> Option<String> {
		const WHY: &str =
			"reconnected to a different node (load balancer, or a node restarted with a new key)";
		if let (Some(a), Some(b)) = (&self.peer_id, &now.peer_id) {
			return (a != b).then(|| format!("{WHY}: its peer ID changed from {a} to {b}"));
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

/// Follows best and finalized blocks; when the connection drops or goes silent, reports it,
/// opens it again (connected means subscribed) and follows on. Blocks below the first new best
/// block after a reconnect are filled in, as after a reorg. A node that answers nothing for the
/// budget is unreachable, one that keeps dropping the connection unstable, one that came back
/// restarted is down, and one that came back as another process a warning.
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
	let (mut drops, mut unstable) = (Drops::default(), false);
	let first = tokio::time::timeout(backoff.attempt, Session::subscribe(client));
	let mut session = match until_stopped(&stop, first).await {
		None => return,
		Some(Ok(session)) => session,
		Some(Err(_)) => Err("subscribing got no answer in time".into()),
	};
	let mut unreachable = false;
	loop {
		let mut alive = Liveness::new();
		let reason = match session {
			Ok(s) => {
				followed.identity.get_or_insert_with(|| s.identity.clone());
				let _ = clients.send(s.client.clone());
				match follow(s, &out, &queue, &stop, &mut followed, &mut alive).await {
					None => return,
					Some(reason) => reason,
				}
			},
			Err(reason) => reason,
		};
		// A silent connection was lost at the node's last answer, and the budget counts from it.
		let silent = alive.silent_since();
		let lost_at = silent.unwrap_or_else(now_ms);
		crate::redact::diag!("blocks: lost the node ({reason}); reconnecting");
		let _ = out.send(BlockEvent::Disconnected { at: lost_at, reason: reason.clone() });
		if !drops.record() && !std::mem::replace(&mut unstable, true) {
			let _ = out.send(BlockEvent::Unstable(format!("blocks: {}", Drops::reason())));
		}
		// Past the budget, the follower keeps trying.
		let mut budget = if silent.is_some() { alive.left_of(&backoff) } else { backoff.clone() };
		let s = loop {
			match reconnect(|| Session::open(&url), &budget, &stop).await {
				Reconnect::Connected(s) => break s,
				Reconnect::Stopped => return,
				Reconnect::GaveUp(why) => {
					if !std::mem::replace(&mut unreachable, true) {
						let why = format!(
							"the block follower lost the node ({reason}), and the node answered nothing for {} s: {why}",
							backoff.total.as_secs()
						);
						crate::redact::diag!("blocks: {why}; still trying");
						let _ = out.send(BlockEvent::Unreachable(why));
					}
					budget = backoff.clone();
				},
			}
		};
		if let Some(id) = followed.identity.as_mut() {
			if let Some(why) = id.restarted(&s.identity) {
				crate::redact::diag!("blocks: {why}");
				let _ = out.send(BlockEvent::Restarted(why));
			} else if let Some(why) = id.other_node(&s.identity) {
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
	/// Who the node was on the last connection.
	identity: Option<Identity>,
}

/// Follows on one session. `None` at `stop`; else why the connection was lost. `alive` is when
/// the node last answered: it asks the node for its best block every [`REQUEST`], and a node that
/// answers nothing for [`SILENT`] loses the connection.
async fn follow(
	session: Session,
	out: &mpsc::UnboundedSender<BlockEvent>,
	queue: &mpsc::UnboundedSender<Queued>,
	stop: &CancellationToken,
	f: &mut Followed,
	alive: &mut Liveness,
) -> Option<String> {
	let Session { client, mut best, mut finalized, .. } = session;
	let mut every = tokio::time::interval(REQUEST);
	every.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
	let mut asked: Option<std::pin::Pin<Box<dyn Future<Output = bool> + Send>>> = None;
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
			_ = every.tick(), if asked.is_none() => {
				let client = client.clone();
				// An error is an answer too: only a read with none counts as silence.
				let read = async move { !matches!(bounded("best number", client.best_number()).await, Err(ChainError::Timeout(_))) };
				asked = Some(Box::pin(read));
			},
			answered = async { asked.as_mut().expect("asked").await }, if asked.is_some() => {
				asked = None;
				if answered {
					alive.answered();
				} else if alive.silent_since().is_some() {
					return Some(format!("the node answered nothing for {} s on an open connection", SILENT.as_secs()));
				}
			},
			block = finalized.next() => match block {
				Some(Ok(b)) => { alive.answered(); let _ = out.send(BlockEvent::Finalized { number: b.number() as u32, seen_at: now_ms() }); }
				Some(Err(e)) => { alive.answered(); let _ = out.send(BlockEvent::Unreadable(format!("finalized: {e}"))); }
				None => return Some("the finalized block stream ended".into()),
			},
			block = best.next() => match block {
				None => return Some("the best block stream ended".into()),
				Some(Err(e)) => {
					alive.answered();
					errors += 1;
					if errors >= ERRORS_IN_A_ROW {
						return Some(format!("{errors} best block errors in a row, the last: {e}"));
					}
					let _ = out.send(BlockEvent::Unreadable(format!("best: {e}")));
				},
				Some(Ok(b)) => {
					alive.answered();
					errors = 0;
					if b.number() as u32 >= first && !f.seen.contains(&b.hash().0) {
						let (number, hash, seen_at) = (b.number() as u32, b.hash().0, now_ms());
						let _ = out.send(BlockEvent::Head { number, seen_at });
						let walk = fill_in(b.header().parent_hash.0, number, first, &f.seen, |h| bounded("parent header", parent(&client, h)));
						match until_stopped(stop, walk).await? {
							Ok(missing) => for (h, n) in missing {
								f.seen.insert(h);
								let _ = queue.send((h, n, seen_at, true));
							},
							Err(e) => { let _ = out.send(BlockEvent::Unreadable(format!("blocks below {number}: {e}"))); }
						}
						f.seen.insert(hash);
						let _ = queue.send((hash, number, seen_at, false));
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

/// The longest one read of a block may take: a full block under load takes a while.
const READ: Duration = Duration::from_secs(30);

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
		.map(|(hash, number, seen_at, filled_in)| {
			let clients = clients.clone();
			async move {
				let mut attempt = 1;
				let result = loop {
					let client = clients.borrow().clone();
					// A read on a connection that went silent would wait for good.
					let read =
						tokio::time::timeout(READ, read(&client, hash, number, seen_at, limit));
					let read = read.await.unwrap_or_else(|_| {
						Err(ChainError::Timeout(format!(
							"block {number}: no answer in {} s",
							READ.as_secs()
						)))
					});
					match read {
						Err(e) if e.fault() != Fault::Tool && attempt < READ_ATTEMPTS => {
							attempt += 1;
							tokio::time::sleep(Duration::from_secs(1)).await;
						},
						result => break result,
					}
				};
				(number, seen_at, filled_in, result)
			}
		})
		.buffered(FETCH_AHEAD);
	// The highest block read, and its `Timestamp.Now`.
	let mut last: Option<(u32, Millis)> = None;
	loop {
		let (number, seen_at, filled_in, result) = tokio::select! {
			() = stop.cancelled() => return,
			Some(r) = reads.next() => r,
		};
		match result {
			Ok((mut record, txs)) => {
				record.fetch_lag_ms = Some(now_ms().saturating_sub(seen_at));
				record.filled_in = filled_in;
				record.gap_ms = chain_gap(last, record.number, record.timestamp);
				if record.timestamp > 0 && last.is_none_or(|(n, _)| record.number > n) {
					last = Some((record.number, record.timestamp));
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

/// The chain's own time per block from `last`, the highest block read before (its number and
/// `Timestamp.Now`), to block `number` with `timestamp`. `None` for a block at or below it (a
/// fork), or without a timestamp.
pub fn chain_gap(last: Option<(u32, Millis)>, number: u32, timestamp: Millis) -> Option<Millis> {
	let (n, at) = last?;
	(number > n && timestamp > 0).then(|| timestamp.saturating_sub(at) / u64::from(number - n))
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

	/// A connection on which the node answered nothing for 10 s is lost from its last answer, and
	/// the reconnect budget counts from then: a node frozen with its socket open is unreachable
	/// 15 s after it last answered, as one that closed it. (The follower waited on it for good: a
	/// node frozen for 16 to 20 s passed.)
	#[tokio::test(start_paused = true)]
	async fn a_silent_connection_is_lost_from_the_last_answer() {
		let mut alive = Liveness::new();
		tokio::time::advance(Duration::from_secs(9)).await;
		assert_eq!(alive.silent_since(), None, "answered 9 s ago");
		alive.answered();
		let answered_at = now_ms();
		tokio::time::advance(SILENT).await;
		assert_eq!(alive.silent_since(), Some(answered_at));
		assert_eq!(alive.left_of(&Backoff::default()).total, Duration::from_secs(5));
	}

	/// A block's gap is the chain's time per block since the highest block read before it.
	#[test]
	fn a_block_gap_is_the_chains_own_time() {
		assert_eq!(chain_gap(None, 10, 60_000), None, "the first block read");
		assert_eq!(chain_gap(Some((10, 60_000)), 11, 66_000), Some(6_000));
		assert_eq!(chain_gap(Some((10, 60_000)), 13, 78_000), Some(6_000), "two filled in");
		assert_eq!(chain_gap(Some((12, 72_000)), 12, 73_000), None, "a fork block");
		assert_eq!(chain_gap(Some((10, 60_000)), 11, 0), None, "no timestamp");
	}

	#[test]
	fn a_reorg_fills_in_the_new_branch() {
		// Read: 10 -> 11 -> 12. The node switches to 13' on 12' on 11' on 10, and announces 13'.
		let chain = HashMap::from([(h(111), h(10)), (h(112), h(111))]);
		let out = fill(&chain, h(112), 13, 5, &[h(10), h(11), h(12)]);
		assert_eq!(out, [(h(111), 11), (h(112), 12)]);
	}
}
