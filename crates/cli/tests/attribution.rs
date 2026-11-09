//! Whose fault it is (docs/reading-results.md). A: each test makes a fault that is not the
//! chain's, as in the faults found on a dev chain behind a TCP proxy, and checks that the run
//! does not end on a chain failure, or show a number it has no evidence for. B: each test makes
//! a fault that is the chain's, alongside one of PolkaMeter's own, and checks that the run still
//! fails. Control cases check that a chain that really fails still does.

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
	rules::{RULES, breaking_point, check_step},
	runner::{Io, Mode, RunOptions, baseline, load, recover, watch},
	sender::{Reply, Sender},
	steps::final_step,
	submit::Submit,
	tracker::Tracker,
};
use polkameter_plugin_sdk::observers::{
	BlockRecord, Phase, Problems, RunDir, now_ms,
	summary::{Baseline, BreakingPoint, Class, Recovery, Rule, Stop},
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
	baseline_probes: usize,
) -> (Tracker<S>, Io, RunOptions) {
	let dir = RunDir::create(Path::new(env!("CARGO_TARGET_TMPDIR")), name).unwrap();
	let tx = |n: u32| Tx::new(n.to_le_bytes().to_vec());
	let (txs, probes) = ((0..5_000).map(tx).collect(), (5_000..5_020).map(tx).collect());
	let mut t = Tracker::new(
		QueueSource::new(txs, probes),
		sender,
		dir.jsonl("load.jsonl").unwrap(),
		dir.jsonl("blocks.jsonl").unwrap(),
		baseline_probes,
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
	};
	let opts = RunOptions {
		rules: RULES,
		mode: Mode::Stress,
		plan: Plan::ramp(rate, 0.0, seconds, 1),
		connections: 1,
		recovery_s: 20,
		baseline_probes,
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
/// `delay_ms` after it was sent. With a [`Link`], it answers and pools each tx 2 s after it was
/// sent instead, and the connection can drop.
struct Node {
	replies: mpsc::UnboundedSender<Reply>,
	pool: Pool,
	link: Option<Arc<Mutex<Link>>>,
}

type Pool = Arc<Mutex<Vec<(TxHash, u64)>>>;

/// Sender connection 0 of a [`Node`]: the requests the node has not read yet, and whether the
/// connection is down.
#[derive(Default)]
struct Link {
	unread: Vec<(u64, TxHash, u64)>,
	down: bool,
}

impl Submit for Node {
	fn submit(&mut self, id: u64, bytes: &[u8]) -> Option<usize> {
		let hash = Tx::new(bytes.to_vec()).hash;
		if let Some(link) = &self.link {
			let mut link = link.lock().unwrap();
			if link.down {
				return None;
			}
			link.unread.push((id, hash, now_ms()));
			return Some(0);
		}
		self.pool.lock().unwrap().push((hash, now_ms()));
		self.replies.send(Reply::Accepted { id, at: now_ms() }).ok()?;
		Some(0)
	}
	fn queued_bytes(&self) -> u64 {
		0
	}
	fn disconnected(&self) -> bool {
		self.link.as_ref().is_some_and(|l| l.lock().unwrap().down)
	}
}

/// What goes wrong in a [`chain_run`]. Seconds count from the start of the run.
#[derive(Clone, Copy)]
enum Fault {
	/// PolkaMeter itself is paused for `.1` ms at `.0` ms (SIGSTOP, CPU starvation): every block
	/// made and every reply sent meanwhile is seen only after it.
	Paused(u64, u64),
	/// The block follower is disconnected from second `.0` to `.1` (an RPC outage, reconnected
	/// within the budget): the blocks made meanwhile are read after the reconnect.
	FollowerDown(u64, u64),
	/// The sender's connection drops at second `.0` and opens again at `.1`; what the node had
	/// not read is sent again then, and only then reaches its pool.
	SenderDown(u64, u64),
	/// None: the chain includes each tx in the first block `.0` ms after it reached the pool;
	/// 11 s is slower than the 10 s p95 limit.
	Slow(u64),
	/// The node reads each submit 2 s after it was sent, and answers then.
	Linked,
	/// The chain finalizes no block from second `.0` on: a finality stall.
	FinalityFrozen(u64),
	/// The chain makes no block from second `.0` on: a production stall.
	Halted(u64),
	/// The follower loses the node at second `.0` and is back 2 s later, with `.1` about who
	/// answers: the same node restarted, or another one.
	Reconnect(u64, Answer),
}

/// Who answers the follower after a [`Fault::Reconnect`].
#[derive(Clone, Copy)]
enum Answer {
	/// The node, restarted: same peer ID, new start block.
	Restarted,
	/// Another node behind a load balancer: a new peer ID.
	OtherNode,
}

/// A chain with a block every 6 s, including what reached `pool` `delay_ms` before, and the
/// faults of `faults` other than [`Fault::SlowChain`].
fn chain(
	pool: Pool,
	link: Option<(Arc<Mutex<Link>>, mpsc::UnboundedSender<Reply>)>,
	faults: &[Fault],
	blocks_tx: mpsc::UnboundedSender<BlockEvent>,
) {
	let (mut delay_ms, mut down, mut sender_down) = (0, None, None);
	let (mut frozen, mut halted, mut reconnect) = (u64::MAX, u64::MAX, None);
	for fault in faults {
		match *fault {
			Fault::Paused(at, ms) => {
				tokio::spawn(async move {
					tokio::time::sleep(Duration::from_millis(at)).await;
					tokio::time::advance(Duration::from_millis(ms)).await;
				});
			},
			Fault::FollowerDown(from, to) => down = Some((from, to)),
			Fault::SenderDown(from, to) => sender_down = Some((from, to)),
			Fault::Slow(ms) => delay_ms = ms,
			Fault::Linked => {},
			Fault::FinalityFrozen(from) => frozen = from,
			Fault::Halted(from) => halted = from,
			Fault::Reconnect(at, answer) => reconnect = Some((at, answer)),
		}
	}
	tokio::spawn(async move {
		let start = Instant::now();
		let (mut number, mut held) = (0, Vec::new());
		for second in 1.. {
			tokio::time::sleep_until(start + Duration::from_secs(second)).await;
			let now = now_ms();
			if let Some((link, replies)) = &link {
				let mut l = link.lock().unwrap();
				let (from, to) = sender_down.unwrap_or((u64::MAX, u64::MAX));
				if second == from {
					l.down = true;
					let ids = l.unread.iter().map(|(id, ..)| *id).collect();
					let _ = replies.send(Reply::Interrupted {
						connection: 0,
						ids,
						reason: "reset".into(),
					});
				}
				if second == to {
					l.down = false;
					let _ = replies
						.send(Reply::Reconnected { connection: 0, down_ms: (to - from) * 1_000 });
				}
				// The node reads a request 2 s after it was sent, or at once when it is sent again.
				let (resent, down) = (second == to, l.down);
				let (read, unread) = l
					.unread
					.drain(..)
					.partition::<Vec<_>, _>(|(.., sent)| !down && (resent || sent + 2_000 <= now));
				l.unread = unread;
				for (id, hash, _) in read {
					pool.lock().unwrap().push((hash, now));
					let _ = replies.send(Reply::Accepted { id, at: now });
				}
			}
			let mut events = Vec::new();
			if let Some((at, answer)) = reconnect {
				if second == at {
					events.push(BlockEvent::Disconnected { at: now, reason: "reset".into() });
				}
				if second == at + 2 {
					events.push(BlockEvent::Reconnected { at: now });
					events.push(match answer {
						Answer::Restarted => BlockEvent::Restarted("the node restarted".into()),
						Answer::OtherNode => BlockEvent::OtherNode("its peer ID changed".into()),
					});
				}
			}
			if down.is_some_and(|(from, _)| from == second) {
				events.push(BlockEvent::Disconnected { at: now, reason: "stream ended".into() });
			}
			if down.is_some_and(|(_, to)| to == second) {
				events.push(BlockEvent::Reconnected { at: now });
				// Read now, as the follower catches up.
				events.extend(held.drain(..).flatten().map(|e| match e {
					BlockEvent::Head { number, .. } => BlockEvent::Head { number, seen_at: now },
					BlockEvent::Fetched { mut record, txs } => {
						record.seen_at = now;
						BlockEvent::Fetched { record, txs }
					},
					BlockEvent::Finalized { number, .. } => {
						BlockEvent::Finalized { number, seen_at: now }
					},
					e => e,
				}));
			}
			if second % 6 == 0 && second < halted {
				number += 1;
				let txs: Vec<(TxHash, bool)> = {
					let mut pool = pool.lock().unwrap();
					let (ready, later) =
						pool.drain(..).partition::<Vec<_>, _>(|(_, sent)| sent + delay_ms <= now);
					*pool = later;
					ready.into_iter().map(|(hash, _)| (hash, false)).collect()
				};
				let mut b = Vec::from(block(number, now, txs));
				if second >= frozen {
					b.retain(|e| !matches!(e, BlockEvent::Finalized { .. }));
				}
				if down.is_some_and(|(from, to)| (from..to).contains(&second)) {
					held.push(b);
				} else {
					events.extend(b);
				}
			}
			for e in events {
				if blocks_tx.send(e).is_err() {
					return;
				}
			}
		}
	});
}

/// The runner on a [`chain`] with `faults`, and `baseline_probes` before the load.
fn on_chain(
	name: &str,
	faults: &[Fault],
	(rate, seconds): (f64, u32),
	baseline_probes: usize,
) -> (Tracker<Node>, Io, RunOptions) {
	let (replies_tx, replies) = mpsc::unbounded_channel();
	let (blocks_tx, blocks) = mpsc::unbounded_channel();
	let pool = Pool::default();
	let link = faults
		.iter()
		.any(|f| matches!(f, Fault::SenderDown(..) | Fault::Linked))
		.then(|| Arc::new(Mutex::new(Link::default())));
	let node = Node { replies: replies_tx.clone(), pool: pool.clone(), link: link.clone() };
	chain(pool, link.map(|l| (l, replies_tx)), faults, blocks_tx);
	let (t, io, mut opts) = runner(name, node, replies, blocks, (rate, seconds), baseline_probes);
	opts.recovery_s = 30;
	(t, io, opts)
}

/// One 30 s step at 20 tx/s with `fault`, then recovery so every inclusion is read: the load's
/// stop and the failure onset.
async fn chain_run(name: &str, fault: Fault) -> (Stop, Option<BreakingPoint>) {
	let (mut t, mut io, opts) = on_chain(name, &[fault], (20.0, 30), 0);
	let stop = load(&mut t, &mut io, &opts).await.unwrap();
	recover(&mut t, &mut io, &opts, Some(12_000)).await.unwrap();
	let finals: Vec<_> = t.steps.iter().map(final_step).collect();
	(stop, breaking_point(&finals, &RULES))
}

/// Block `number` as the follower reads it at `seen_at`: new best, read, finalized at once.
fn block(number: u32, seen_at: u64, txs: Vec<(TxHash, bool)>) -> [BlockEvent; 3] {
	let record = BlockRecord { number, seen_at, gap_ms: Some(6_000), ..Default::default() };
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
	let (stop, onset) = chain_run("attribution-paused", Fault::Paused(10_000, 8_000)).await;
	assert_eq!((stop.rule, stop.class), (Rule::GeneratorLimit, None), "{}", stop.detail);
	assert_eq!(onset, None, "a pause of PolkaMeter is not the chain's latency");

	let (stop, onset) = chain_run("attribution-follower", Fault::FollowerDown(9, 24)).await;
	assert_eq!(stop.rule, Rule::RateCap, "{}", stop.detail);
	assert_eq!(onset, None, "blocks read after a reconnect are not the chain's latency");

	let (stop, onset) = chain_run("attribution-slow-chain", Fault::Slow(11_000)).await;
	assert_eq!(stop.rule, Rule::RateCap, "{}", stop.detail);
	assert_eq!(onset.map(|o| o.measure), Some("latency".into()), "a slow chain still fails");
}

/// I4. The sender's connection drops for 13 s: the node had not read the txs it had unanswered,
/// and gets them only when they are sent again. Their time to inclusion holds the outage, which
/// is not the chain's latency, so it makes no failure onset. (It did: p95 above 10 s.)
#[tokio::test(start_paused = true)]
async fn i4_time_to_resend_after_a_reconnect_is_not_latency() {
	let (stop, onset) = chain_run("attribution-resent", Fault::SenderDown(9, 22)).await;
	assert_eq!((stop.rule, stop.class), (Rule::GeneratorLimit, None), "{}", stop.detail);
	assert_eq!(onset, None, "a tx sent again after a reconnect is not the chain's latency");
}

/// Baseline, a 6 s load at 2 tx/s and recovery, with PolkaMeter paused at the seconds of
/// `pauses`: the baseline and the recovery.
async fn phases_run(name: &str, pauses: &[Fault]) -> (Baseline, Recovery) {
	let (mut t, mut io, opts) = on_chain(name, pauses, (2.0, 6), 3);
	let base = baseline(&mut t, &mut io, &opts).await.unwrap();
	load(&mut t, &mut io, &opts).await.unwrap();
	(base, recover(&mut t, &mut io, &opts, Some(12_000)).await.unwrap())
}

/// I3 in the baseline and in recovery. Paused in the baseline, PolkaMeter does not take the
/// probe it timed across the pause as the chain's latency. Paused in recovery, it does not
/// report the time the pause took as the chain's recovery: the recovery is not measured. (It
/// reported a 10 s baseline probe, and recovery after 18 s, on a chain back after 6 s.)
#[tokio::test(start_paused = true)]
async fn i3_a_pause_in_the_baseline_or_recovery_is_not_the_chains() {
	let (base, r) = phases_run("attribution-phases", &[]).await;
	assert!(base.max_ms.is_some_and(|ms| ms <= 6_000), "{base:?}");
	assert!(r.measured && r.recovered && r.seconds <= Some(6), "{r:?}");

	let pauses = [Fault::Paused(8_000, 8_000), Fault::Paused(33_000, 15_000)];
	let (base, r) = phases_run("attribution-phases-paused", &pauses).await;
	assert!(base.max_ms.is_none_or(|ms| ms <= 6_000), "the probe timed across the pause: {base:?}");
	assert!(!r.measured, "a pause in recovery is not the chain's recovery time: {r:?}");
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
		runner("attribution-reconnect", sender, replies, blocks, (100.0, 3), 0);
	// No blocks in 3 s: keep the stall rules quiet.
	t.last_head.0 = now_ms() + 60_000;
	t.last_finalized.0 = now_ms() + 60_000;
	let stop = load(&mut t, &mut io, &opts).await.unwrap();
	let gap = t.observation_gaps.iter().find(|g| g.what == "sender connection 0");
	assert!(gap.is_some_and(|g| g.to.is_some()), "the connection dropped and reconnected");
	assert_ne!(stop.rule, Rule::PoolIntake, "{}", stop.detail);
	assert_eq!((stop.rule, stop.class), (Rule::GeneratorLimit, None), "{}", stop.detail);
}

// A. Never blame the chain for a fault outside it.

/// A1. PolkaMeter paused for 4.9 s, under the 5 s that stops the load at once, just before a
/// block: the txs of that block are seen 4.9 s late. That is not the chain's latency, whatever
/// the pause's length: no failure onset. The step sent too little, so the load ends on
/// "generator limit". (A latency onset made it FAIL: p95 above 10 s on a chain at 3 to 9 s.)
#[tokio::test(start_paused = true)]
async fn a_pause_of_any_length_never_makes_a_latency_failure() {
	let faults = [Fault::Slow(3_000), Fault::Paused(11_500, 4_900)];
	let (mut t, mut io, opts) = on_chain("attribution-short-pause", &faults, (20.0, 30), 0);
	let stop = load(&mut t, &mut io, &opts).await.unwrap();
	recover(&mut t, &mut io, &opts, Some(12_000)).await.unwrap();
	let finals: Vec<_> = t.steps.iter().map(final_step).collect();
	assert_eq!(stop.class, None, "{}: {}", stop.rule.name(), stop.detail);
	assert_eq!(breaking_point(&finals, &RULES), None, "{:?}", finals[0].p95_latency_ms);
	assert!(finals[0].untimed > 0, "the txs seen late are untimed");
}

/// A2. PolkaMeter paused for 4.9 s while the node answered submits 2 s after they were sent:
/// the replies read after the pause hold it. That is not the node's reply time: no "pool
/// intake". (It was: p95 submit reply above 5 s.)
#[tokio::test(start_paused = true)]
async fn a_pause_is_never_the_nodes_reply_time() {
	let faults = [Fault::Linked, Fault::Paused(9_500, 4_900)];
	let (mut t, mut io, opts) = on_chain("attribution-reply-pause", &faults, (20.0, 30), 0);
	let stop = load(&mut t, &mut io, &opts).await.unwrap();
	assert_eq!(stop.class, None, "{}: {}", stop.rule.name(), stop.detail);
	let f = final_step(&t.steps[0]);
	assert!(f.p95_reply_ms.is_some_and(|ms| ms <= 3_000), "{:?}", f.p95_reply_ms);
	assert!(f.max_oldest_pending_ms <= 3_000, "{}", f.max_oldest_pending_ms);
}

/// A3. PolkaMeter paused for 18 s in a step, and the node sent one new head for the blocks it
/// made meanwhile. The pause is not a block gap: the step's blocks are not slow. (They were: a
/// mean gap of 15 s against 6 s.)
#[test]
fn a_pause_never_counts_in_block_gaps() {
	let (_replies_tx, replies) = mpsc::unbounded_channel();
	let (_blocks_tx, blocks) = mpsc::unbounded_channel();
	let node = Node { replies: mpsc::unbounded_channel().0, pool: Pool::default(), link: None };
	let (mut t, ..) = runner("attribution-gap-pause", node, replies, blocks, (1.0, 30), 0);
	let start = now_ms();
	t.start_step(0, 1.0, start).unwrap();
	let fetched = |number, seen_at, gap_ms| {
		let record = BlockRecord { number, seen_at, gap_ms, ..Default::default() };
		BlockEvent::Fetched { record, txs: vec![] }
	};
	t.on_block(BlockEvent::Head { number: 1, seen_at: start + 6_000 }, start + 6_000)
		.unwrap();
	t.on_block(fetched(1, start + 6_000, None), start + 6_000).unwrap();
	t.paused(start + 7_000, start + 25_000);
	t.on_block(BlockEvent::Head { number: 4, seen_at: start + 25_100 }, start + 25_100)
		.unwrap();
	t.on_block(fetched(4, start + 25_100, Some(19_100)), start + 25_100).unwrap();
	t.end_step(start + 30_000);
	let f = final_step(&t.steps[0]);
	assert!(f.blocks.mean_block_gap_ms.is_some_and(|g| g <= 6_000.0), "{:?}", f.blocks);
	assert!(f.blocks.max_block_gap_ms.is_some_and(|g| g <= 6_000), "{:?}", f.blocks);
	assert_eq!(t.steps[0].blocks[1].gap_ms, None, "a gap with the pause in it is not known");
	let stop = check_step(&t.steps, t.source(), 0, 6_000.0, &RULES, true).unwrap();
	assert_ne!(stop.rule, Rule::SlowBlocks, "{}", stop.detail);
}

/// A4. A tx PolkaMeter could not time (here: read as the follower caught up after a reconnect)
/// has no time in any export: no inclusion time in the ledger, no sample in the inclusion
/// histogram, and a samples.jtl row apart from the step's timed ones. (They kept the time.)
#[tokio::test(start_paused = true)]
async fn untimed_transactions_have_no_time_in_any_export() {
	let (mut t, mut io, opts) =
		on_chain("attribution-exports", &[Fault::FollowerDown(9, 24)], (20.0, 30), 0);
	load(&mut t, &mut io, &opts).await.unwrap();
	recover(&mut t, &mut io, &opts, Some(12_000)).await.unwrap();
	let untimed: Vec<_> = t.flood.values().filter(|tx| tx.untimed).collect();
	assert!(!untimed.is_empty());
	for tx in untimed {
		assert_eq!(tx.inclusion_ms, None, "an untimed tx keeps no inclusion time");
	}
	let st = &t.steps[0];
	assert_eq!(st.latencies_ms.len() as u64, st.included - st.untimed);

	let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("attribution-jtl");
	std::fs::create_dir_all(&dir).unwrap();
	std::fs::write(dir.join("events.jsonl"), "").unwrap();
	let rows = [
		r#"{"hash":"aa","step":0,"sentAt":1,"replyMs":5,"inclusionMs":900,"untimed":false,"status":"finalized","failedInFinalizedBlock":false}"#,
		r#"{"hash":"bb","step":0,"sentAt":2,"replyMs":5,"inclusionMs":null,"untimed":true,"status":"finalized","failedInFinalizedBlock":false}"#,
	];
	std::fs::write(dir.join("transactions.jsonl"), rows.join("\n") + "\n").unwrap();
	polkameter::engine::artifacts::write_samples(&dir).unwrap();
	let jtl = std::fs::read_to_string(dir.join("samples.jtl")).unwrap();
	let cells: Vec<Vec<&str>> = jtl.lines().skip(1).map(|l| l.split(',').collect()).collect();
	assert_eq!((cells[0][1], cells[0][2]), ("900", "load:step:0"), "{jtl}");
	assert_eq!((cells[1][1], cells[1][2]), ("0", "load:step:0:untimed"), "{jtl}");
}

/// A5. Baseline probes all landed while PolkaMeter was paused: the baseline has no time, never
/// 0 ms. (It was 0 ms, and passed.)
#[tokio::test(start_paused = true)]
async fn a_baseline_polkameter_did_not_observe_has_no_result() {
	// Each probe is sent on a new block, and PolkaMeter pauses before the next one lands it.
	let pauses =
		[Fault::Paused(6_500, 6_000), Fault::Paused(13_000, 6_000), Fault::Paused(20_000, 6_000)];
	let (base, _) = phases_run("attribution-baseline-paused", &pauses).await;
	let json = serde_json::to_value(&base).unwrap();
	assert_eq!(base.probes, 3);
	assert!(json["p50Ms"].is_null() && json["maxMs"].is_null(), "{json}");
}

/// A6. The block follower lost the node for the whole recovery and was back within the budget:
/// PolkaMeter saw no block, so the recovery is not measured, never a recovery that failed. A
/// chain that makes no block while PolkaMeter watches still fails it (control). (It failed:
/// "no new block in 20 s".)
#[tokio::test(start_paused = true)]
async fn a_recovery_polkameter_did_not_observe_is_not_measured() {
	for (name, fault, measured) in [
		("attribution-recovery-unobserved", Fault::FollowerDown(7, 30), false),
		("attribution-recovery-halted", Fault::Halted(7), true),
	] {
		let (mut t, mut io, mut opts) = on_chain(name, &[fault], (2.0, 6), 0);
		opts.recovery_s = 20;
		load(&mut t, &mut io, &opts).await.unwrap();
		let r = recover(&mut t, &mut io, &opts, Some(12_000)).await.unwrap();
		assert_eq!((r.measured, r.recovered), (measured, false), "{name}: {r:?}");
	}
}

/// A7. The RPC connection dropped again and again, each time back at once, and the node did not
/// restart: the connection is unstable. The load ends without a chain failure, on "generator
/// limit". (It was "node down": more than 5 drops in 60 s.)
#[tokio::test]
async fn a_connection_that_keeps_dropping_is_unstable_not_node_down() {
	let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
	let url = format!("ws://{}", listener.local_addr().unwrap());
	tokio::spawn(async move {
		loop {
			let (tcp, _) = listener.accept().await.unwrap();
			// Each connection answers submits for 100 ms, then drops.
			tokio::spawn(async move {
				let _ = tokio::time::timeout(
					Duration::from_millis(100),
					serve(tcp, Instant::now() + Duration::from_secs(60)),
				)
				.await;
			});
		}
	});
	let (replies_tx, replies) = mpsc::unbounded_channel();
	let backoff = Backoff {
		waits: vec![Duration::from_millis(20)],
		attempt: Duration::from_secs(1),
		total: Duration::from_secs(15),
	};
	let sender = Sender::open(&url, 1, replies_tx, &backoff).await.unwrap();
	let (_blocks_tx, blocks) = mpsc::unbounded_channel();
	let (mut t, mut io, opts) =
		runner("attribution-unstable", sender, replies, blocks, (50.0, 5), 0);
	t.last_head.0 = now_ms() + 60_000;
	t.last_finalized.0 = now_ms() + 60_000;
	let stop = load(&mut t, &mut io, &opts).await.unwrap();
	assert_eq!((stop.rule, stop.class), (Rule::GeneratorLimit, None), "{}", stop.detail);
	assert!(stop.detail.contains("connection unstable"), "{}", stop.detail);
}

/// A8. The loss check's connection dropped and the node was back 8 s later, within the budget:
/// the read is made again on a new connection, and gets its answer. (It was not: 48 txs were
/// left unverified.)
#[tokio::test(start_paused = true)]
async fn the_loss_check_reconnects_within_the_budget() {
	use polkameter::load::loss::Live;
	use polkameter_plugin_sdk::chain::ChainError;
	let start = Instant::now();
	let back = start + Duration::from_secs(8);
	// A connection opened before the outage fails every read; the node answers on one opened
	// after it is back.
	let connect = || async move {
		if Instant::now() < back {
			Err(ChainError::Timeout("connection refused".into()))
		} else {
			Ok(true)
		}
	};
	let mut live = Live::new(false, connect, Backoff::default());
	let read = |up: bool| async move {
		if up { Ok(42) } else { Err(ChainError::Timeout("the connection closed".into())) }
	};
	assert_eq!(live.read(read).await.unwrap(), 42);
	assert!(start.elapsed() <= Duration::from_secs(15), "{:?}", start.elapsed());
	assert!(*live.conn(), "reads go on on the new connection");
}

/// A10. The sender's connection dropped with a tx unanswered; the node had it, put it in a block,
/// and refused the copy sent after the reconnect as "outdated". That refusal is no evidence
/// against the chain: the tx counts as included when its block is read. (It was refused, and
/// never included: a failure onset on a chain that included every tx.)
#[test]
fn a_refused_resend_is_not_a_refusal() {
	let (_replies_tx, replies) = mpsc::unbounded_channel();
	let (_blocks_tx, blocks) = mpsc::unbounded_channel();
	let link = Some(Arc::new(Mutex::new(Link::default())));
	let node = Node { replies: mpsc::unbounded_channel().0, pool: Pool::default(), link };
	let (mut t, ..) = runner("attribution-resend-refused", node, replies, blocks, (1.0, 30), 0);
	let start = now_ms();
	t.start_step(0, 1.0, start).unwrap();
	assert!(t.send_next(start));
	t.on_reply(Reply::Interrupted { connection: 0, ids: vec![1], reason: "reset".into() });
	t.on_reply(Reply::Reconnected { connection: 0, down_ms: 1_000 });
	let error = "Invalid Transaction: Transaction is outdated".to_owned();
	t.on_reply(Reply::Refused { id: 1, at: start + 1_001, code: 1010, error });
	let hash = Tx::new(0u32.to_le_bytes().to_vec()).hash;
	let record = BlockRecord { number: 1, seen_at: start + 6_000, ..Default::default() };
	t.on_block(BlockEvent::Fetched { record, txs: vec![(hash, false)] }, start + 6_000)
		.unwrap();
	let st = &t.steps[0];
	assert_eq!((st.included, st.rejected), (1, 0));
	assert!(!t.flood[&hash].refused);
}

/// A9. The follower reconnects with the node unchanged (a proxy or the network dropped), or to
/// another node behind a load balancer: neither is node down. PolkaMeter tells them from the
/// node's own identity, never from the process on the endpoint's port, which may be the proxy.
/// (A restarted proxy was "node down: the node restarted".)
#[tokio::test(start_paused = true)]
async fn a_restarted_proxy_is_not_a_restarted_node() {
	use polkameter::load::follower::Identity;
	let node = Identity { peer_id: Some("12D3KooWNode".into()), starting_block: Some(10) };
	let other = Identity { peer_id: Some("12D3KooWOther".into()), starting_block: Some(50) };
	assert_eq!(node.restarted(&node.clone()), None, "a proxy restart changes nothing");
	assert_eq!(node.other_node(&node.clone()), None);
	assert_eq!(node.restarted(&other), None, "another node did not restart");
	assert!(node.other_node(&other).is_some());

	let (stop, _) =
		chain_run("attribution-other-node", Fault::Reconnect(10, Answer::OtherNode)).await;
	assert_eq!(stop.rule, Rule::RateCap, "{}", stop.detail);
}

// B. Never excuse the chain for a fault in it.

/// B1. The node restarted (the same peer ID, a new sync starting block) while every
/// connection reconnected within the budget: that is node down, in the load and in recovery.
/// (It was a warning, and the load ended on "generator limit".)
#[tokio::test(start_paused = true)]
async fn a_restarted_node_is_node_down_in_any_phase() {
	use polkameter::load::follower::Identity;
	let node = Identity { peer_id: Some("12D3KooWNode".into()), starting_block: Some(10) };
	let restarted = Identity { starting_block: Some(60), ..node.clone() };
	assert!(node.restarted(&restarted).is_some_and(|why| why.contains("restarted")));

	let (stop, _) =
		chain_run("attribution-restarted", Fault::Reconnect(10, Answer::Restarted)).await;
	assert_eq!((stop.rule, stop.class), (Rule::NodeDown, Some(Class::Hard)), "{}", stop.detail);

	let faults = [Fault::Reconnect(10, Answer::Restarted)];
	let (mut t, mut io, opts) = on_chain("attribution-restarted-recovery", &faults, (2.0, 6), 0);
	load(&mut t, &mut io, &opts).await.unwrap();
	let r = recover(&mut t, &mut io, &opts, Some(12_000)).await.unwrap();
	assert!(r.measured && !r.recovered && r.detail.starts_with("node down"), "{r:?}");
}

/// B2. Finality stopped in recovery, longer than the 60 s limit, while PolkaMeter was also
/// paused for 7 s: the chain failed, and a pause of PolkaMeter does not excuse it. (The
/// recovery was "not measured", and the run INCONCLUSIVE.)
#[tokio::test(start_paused = true)]
async fn a_finality_stall_in_recovery_fails_even_when_polkameter_paused() {
	let faults = [Fault::Slow(13_000), Fault::FinalityFrozen(7), Fault::Paused(30_000, 7_000)];
	let (mut t, mut io, mut opts) = on_chain("attribution-recovery-finality", &faults, (2.0, 6), 0);
	opts.recovery_s = 120;
	load(&mut t, &mut io, &opts).await.unwrap();
	let r = recover(&mut t, &mut io, &opts, Some(12_000)).await.unwrap();
	assert!(r.measured && !r.recovered, "{r:?}");
	assert!(r.detail.starts_with("finality stall"), "{}", r.detail);
}

/// B3. Finality stopped for 90 s while the loss check waited for it, and then went on, so every
/// tx was verified: that is still a finality stall, as in the load. (The run passed.)
#[tokio::test(start_paused = true)]
async fn a_finality_stall_while_the_loss_check_runs_fails_the_run() {
	let faults = [Fault::FinalityFrozen(12)];
	let (mut t, mut io, opts) = on_chain("attribution-reconcile-finality", &faults, (2.0, 6), 0);
	load(&mut t, &mut io, &opts).await.unwrap();
	recover(&mut t, &mut io, &opts, Some(12_000)).await.unwrap();
	let wait = tokio::time::sleep(Duration::from_secs(90));
	let ((), failed) = watch(&mut t, &mut io, &opts, wait).await.unwrap();
	let failed = failed.expect("a finality stall");
	assert_eq!(
		(failed.rule, failed.class),
		(Rule::FinalityStall, Some(Class::Hard)),
		"{}",
		failed.detail
	);
}
