//! What PolkaMeter never blames on the chain (docs/reading-results.md). Each test makes a fault
//! that is not the chain's, as in the faults found on a dev chain behind a TCP proxy, and checks
//! that the run does not end on a chain failure. A control case checks that a chain that really
//! fails still does.

use std::{
	path::Path,
	sync::{Arc, Mutex},
	time::Duration,
};

use futures_util::{SinkExt, StreamExt};
use polkameter::load::{
	Plan, QueueSource, Tx, TxHash,
	follower::BlockEvent,
	reconnect::{Backoff, Reconnect, reconnect},
	rules::{RULES, breaking_point},
	runner::{Io, Mode, RunOptions, load, recover},
	sender::{Reply, Sender},
	steps::final_step,
	submit::Submit,
	tracker::Tracker,
};
use polkameter_plugin_sdk::observers::{
	BlockRecord, Phase, Problems, RunDir, now_ms,
	summary::{BreakingPoint, Rule, Stop},
};
use tokio::{sync::mpsc, time::Instant};
use tokio_tungstenite::tungstenite::Message;
use tokio_util::sync::CancellationToken;

/// A tracker in the load with `flood` txs, sending through `sender`, and the runner's inputs: one
/// step of `seconds` at `rate` tx/s.
fn runner<S: Submit>(
	name: &str,
	sender: S,
	replies: mpsc::UnboundedReceiver<Reply>,
	blocks: mpsc::UnboundedReceiver<BlockEvent>,
	(rate, seconds): (f64, u32),
) -> (Tracker<S>, Io, RunOptions) {
	let dir = RunDir::create(Path::new(env!("CARGO_TARGET_TMPDIR")), name).unwrap();
	let txs = (0..5_000u32).map(|n| Tx::new(n.to_le_bytes().to_vec())).collect();
	let mut t = Tracker::new(
		QueueSource::new(txs, vec![]),
		sender,
		dir.jsonl("load.jsonl").unwrap(),
		dir.jsonl("blocks.jsonl").unwrap(),
		0,
		now_ms(),
	);
	t.set_phase(Phase::Ramp, now_ms()).unwrap();
	let (events, _) = tokio::sync::broadcast::channel(64);
	let io = Io {
		replies,
		blocks,
		events,
		problems: Problems::default(),
		tool_failed: CancellationToken::new(),
		boot: || Some(now_ms()),
		restarts: Problems::default(),
	};
	let opts = RunOptions {
		rules: RULES,
		mode: Mode::Stress,
		plan: Plan::ramp(rate, 0.0, seconds, 1),
		connections: 1,
		recovery_s: 20,
		baseline_probes: 0,
		block_interval_s: 6.0,
	};
	(t, io, opts)
}

/// I1. A node that is back within the reconnect budget (15 s) is reconnected, also when it is
/// back only near the end; one unreachable for the whole budget is down, and not before.
/// (An 8 s outage was "node down" after 7.8 s: the backoff gave up when its next wait passed the
/// deadline.)
#[tokio::test(start_paused = true)]
async fn i1_a_node_back_within_the_reconnect_budget_is_not_down() {
	let stop = CancellationToken::new();
	for back_after in [8.0, 14.9] {
		let start = Instant::now();
		let back = start + Duration::from_secs_f64(back_after);
		let attempt = || async move {
			if Instant::now() < back { Err("connection refused") } else { Ok(()) }
		};
		let r = reconnect(attempt, &Backoff::default(), &stop).await;
		assert_eq!(r, Reconnect::Connected(()), "back after {back_after} s");
		assert!(start.elapsed() <= Duration::from_secs(15), "{:?}", start.elapsed());
	}
	let start = Instant::now();
	let never = || async { Err::<(), _>("connection refused") };
	let r = reconnect(never, &Backoff::default(), &stop).await;
	assert!(matches!(r, Reconnect::GaveUp(_)), "{r:?}");
	assert!(start.elapsed() >= Duration::from_secs(15), "gave up after {:?}", start.elapsed());
}

/// A node that accepts every tx at once; the chain includes it in the first block made
/// `delay_ms` after it was sent.
struct Node {
	replies: mpsc::UnboundedSender<Reply>,
	pool: Arc<Mutex<Vec<(TxHash, u64)>>>,
}

impl Submit for Node {
	fn submit(&mut self, id: u64, bytes: &[u8]) -> Option<usize> {
		self.pool.lock().unwrap().push((Tx::new(bytes.to_vec()).hash, now_ms()));
		self.replies.send(Reply::Accepted { id, at: now_ms() }).ok()?;
		Some(0)
	}
	fn queued_bytes(&self) -> u64 {
		0
	}
}

/// What goes wrong in a [`chain_run`].
#[derive(Clone, Copy)]
enum Fault {
	/// PolkaMeter itself is paused for `.1` s at second `.0` (SIGSTOP, CPU starvation): every
	/// block made meanwhile is seen only after it.
	Paused(u64, u64),
	/// The block follower is disconnected from second `.0` to `.1` (an RPC outage, reconnected
	/// within the budget): the blocks made meanwhile are read after the reconnect.
	FollowerDown(u64, u64),
	/// None: the chain includes each tx 11 s after it was sent, slower than the 10 s p95 limit.
	SlowChain,
}

/// One 30 s step at 20 tx/s on a chain with a block every 6 s and `fault`, then recovery so
/// every inclusion is read: the load's stop and the failure onset.
async fn chain_run(name: &str, fault: Fault) -> (Stop, Option<BreakingPoint>) {
	let (replies_tx, replies) = mpsc::unbounded_channel();
	let (blocks_tx, blocks) = mpsc::unbounded_channel();
	let pool = Arc::new(Mutex::new(Vec::new()));
	let node = Node { replies: replies_tx, pool: pool.clone() };
	let (mut t, mut io, opts) = runner(name, node, replies, blocks, (20.0, 30));
	let delay_ms = if matches!(fault, Fault::SlowChain) { 11_000 } else { 0 };
	let down = match fault {
		Fault::FollowerDown(from, to) => Some((from, to)),
		_ => None,
	};
	if let Fault::Paused(at, seconds) = fault {
		tokio::spawn(async move {
			tokio::time::sleep(Duration::from_secs(at)).await;
			tokio::time::advance(Duration::from_secs(seconds)).await;
		});
	}
	tokio::spawn(async move {
		let start = Instant::now();
		let (mut number, mut held) = (0, Vec::new());
		for second in 1.. {
			tokio::time::sleep_until(start + Duration::from_secs(second)).await;
			let now = now_ms();
			let mut events = Vec::new();
			if down.is_some_and(|(from, _)| from == second) {
				events.push(BlockEvent::Disconnected { at: now, reason: "stream ended".into() });
			}
			if down.is_some_and(|(_, to)| to == second) {
				events.push(BlockEvent::Reconnected { at: now });
				events.extend(held.drain(..).flat_map(|(n, txs)| block(n, now, txs)));
			}
			if second % 6 == 0 {
				number += 1;
				let txs: Vec<(TxHash, bool)> = {
					let mut pool = pool.lock().unwrap();
					let (ready, later) =
						pool.drain(..).partition::<Vec<_>, _>(|(_, sent)| sent + delay_ms <= now);
					*pool = later;
					ready.into_iter().map(|(hash, _)| (hash, false)).collect()
				};
				if down.is_some_and(|(from, to)| (from..to).contains(&second)) {
					held.push((number, txs));
				} else {
					events.extend(block(number, now, txs));
				}
			}
			for e in events {
				if blocks_tx.send(e).is_err() {
					return;
				}
			}
		}
	});
	let stop = load(&mut t, &mut io, &opts).await.unwrap();
	recover(&mut t, &mut io, &opts, 12_000).await.unwrap();
	let finals: Vec<_> = t.steps.iter().map(final_step).collect();
	(stop, breaking_point(&finals, &RULES))
}

/// Block `number` as the follower reads it at `seen_at`: new best, read, finalized at once.
fn block(number: u32, seen_at: u64, txs: Vec<(TxHash, bool)>) -> [BlockEvent; 3] {
	let record = BlockRecord { number, seen_at, ..Default::default() };
	[
		BlockEvent::Head { number, seen_at },
		BlockEvent::Fetched { record, txs },
		BlockEvent::Finalized { number, seen_at },
	]
}

/// I3. PolkaMeter paused for 8 s (the chain fine) ends on "generator limit" and the txs it timed
/// across the pause make no failure onset: the verdict is INCONCLUSIVE, not FAIL. The same for
/// a block follower that reconnected: what it read late is not the chain's latency. A chain that
/// really is slow still breaks the latency measure.
#[tokio::test(start_paused = true)]
async fn i3_time_polkameter_did_not_observe_never_makes_a_latency_failure() {
	let (stop, onset) = chain_run("attribution-paused", Fault::Paused(10, 8)).await;
	assert_eq!((stop.rule, stop.class), (Rule::GeneratorLimit, None), "{}", stop.detail);
	assert_eq!(onset, None, "a pause of PolkaMeter is not the chain's latency");

	let (stop, onset) = chain_run("attribution-follower", Fault::FollowerDown(9, 24)).await;
	assert_eq!(stop.rule, Rule::RateCap, "{}", stop.detail);
	assert_eq!(onset, None, "blocks read after a reconnect are not the chain's latency");

	let (stop, onset) = chain_run("attribution-slow-chain", Fault::SlowChain).await;
	assert_eq!(stop.rule, Rule::RateCap, "{}", stop.detail);
	assert_eq!(onset.map(|o| o.measure), Some("latency".into()), "a slow chain still fails");
}

/// When the fake node of [`i4_a_reconnecting_sender_is_never_pool_intake`] is unreachable.
const OUTAGE: std::ops::Range<Duration> = Duration::from_millis(500)..Duration::from_millis(1_500);

/// Answers every submit on `tcp` until the outage starts, then drops the connection.
async fn serve(tcp: tokio::net::TcpStream, start: Instant) {
	let Ok(mut ws) = tokio_tungstenite::accept_async(tcp).await else { return };
	let outage = start + OUTAGE.start;
	loop {
		let msg = tokio::select! {
			() = tokio::time::sleep_until(outage), if Instant::now() < outage => return,
			msg = ws.next() => msg,
		};
		let Some(Ok(msg)) = msg else { return };
		if msg.is_text() {
			let v: serde_json::Value = serde_json::from_str(msg.to_text().unwrap()).unwrap();
			let answer = format!(r#"{{"jsonrpc":"2.0","id":{},"result":"0x00"}}"#, v["id"]);
			if ws.send(Message::text(answer)).await.is_err() {
				return;
			}
		}
	}
}

/// I4. The RPC connection drops for a second and reconnects (a proxy restart), with the node
/// reading every submit: the step sends less, but the node did not stop reading submits, so
/// this is not "pool intake". (It was, with "0 KiB unsent".)
#[tokio::test]
async fn i4_a_reconnecting_sender_is_never_pool_intake() {
	let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
	let url = format!("ws://{}", listener.local_addr().unwrap());
	let start = Instant::now();
	tokio::spawn(async move {
		loop {
			let (tcp, _) = listener.accept().await.unwrap();
			// During the outage every connection is closed before its handshake.
			if !OUTAGE.contains(&start.elapsed()) {
				tokio::spawn(serve(tcp, start));
			}
		}
	});
	let (replies_tx, replies) = mpsc::unbounded_channel();
	let backoff = Backoff {
		waits: vec![Duration::from_millis(50)],
		attempt: Duration::from_secs(1),
		total: Duration::from_secs(15),
	};
	let sender = Sender::open(&url, 1, replies_tx, &backoff).await.unwrap();
	let (_blocks_tx, blocks) = mpsc::unbounded_channel();
	let (mut t, mut io, opts) =
		runner("attribution-reconnect", sender, replies, blocks, (100.0, 3));
	// No blocks in 3 s: keep the stall rules quiet.
	t.last_head.0 = now_ms() + 60_000;
	t.last_finalized.0 = now_ms() + 60_000;
	let stop = load(&mut t, &mut io, &opts).await.unwrap();
	let gap = t.observation_gaps.iter().find(|g| g.what == "sender connection 0");
	assert!(gap.is_some_and(|g| g.to.is_some()), "the connection dropped and reconnected");
	assert_ne!(stop.rule, Rule::PoolIntake, "{}", stop.detail);
	assert_eq!((stop.rule, stop.class), (Rule::GeneratorLimit, None), "{}", stop.detail);
}
