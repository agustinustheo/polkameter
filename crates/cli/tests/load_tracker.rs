//! The tracker as a state machine: a fake sender, hand-made replies and block events, no network.

use polkameter::load::{
	QueueSource, Tx, follower::BlockEvent, sender::Reply, submit::Submit, tracker::Tracker,
};
use polkameter_plugin_sdk::observers::{
	BlockRecord, Phase, RunDir,
	summary::{Outcome, Rule},
};

#[derive(Default)]
struct FakeSender {
	/// The bytes queued, in order.
	bytes: Vec<Vec<u8>>,
	/// Submits still to refuse as backed up.
	full: usize,
}

impl Submit for FakeSender {
	fn submit(&mut self, _id: u64, bytes: &[u8]) -> Option<usize> {
		if self.full > 0 {
			self.full -= 1;
			return None;
		}
		self.bytes.push(bytes.to_vec());
		Some(0)
	}
	fn queued_bytes(&self) -> u64 {
		0
	}
}

fn tx(n: u8) -> Tx {
	Tx::new(vec![n; 8])
}

/// A tracker whose sender refuses its first `full` submits as backed up.
fn tracker(name: &str, flood: Vec<Tx>, probes: Vec<Tx>, full: usize) -> Tracker<FakeSender> {
	let dir = RunDir::create(std::path::Path::new(env!("CARGO_TARGET_TMPDIR")), name).unwrap();
	Tracker::new(
		QueueSource::new(flood, probes),
		FakeSender { full, ..FakeSender::default() },
		dir.jsonl("load.jsonl").unwrap(),
		dir.jsonl("blocks.jsonl").unwrap(),
		1,
		0,
	)
}

fn fetched(number: u32, seen_at: u64, txs: &[&Tx]) -> BlockEvent {
	let record = BlockRecord { number, seen_at, ..Default::default() };
	BlockEvent::Fetched { record, txs: txs.iter().map(|t| (t.hash, false)).collect() }
}

#[test]
fn a_reply_after_inclusion_still_counts_and_a_late_refusal_is_ignored() {
	let (a, b) = (tx(1), tx(2));
	let mut t = tracker("late-reply", vec![a.clone(), b.clone()], vec![], 0);
	t.set_phase(Phase::Ramp, 1_000).unwrap();
	t.start_step(0, 10.0, 1_000).unwrap();
	assert!(t.send_next(1_000) && t.send_next(1_000));
	t.on_block(fetched(5, 1_500, &[&a, &b]), 1_500).unwrap();
	t.on_reply(Reply::Accepted { id: 1, at: 1_600 });
	t.on_reply(Reply::Refused {
		id: 2,
		at: 1_700,
		code: 1014,
		error: "Priority is too low".into(),
	});
	let st = &t.steps[0];
	assert_eq!((st.included, st.rejected), (2, 0), "included twice, the late refusal ignored");
	assert!(!t.flood[&b.hash].refused, "a late refusal does not flag an included tx");
	assert!(t.flood[&a.hash].accepted, "the accepted reply counts");
	assert_eq!(st.reply_ms, vec![600, 700], "both reply times counted");
	assert_eq!(st.blocks.len(), 1);
	assert_eq!(st.blocks[0].ours, 2);
}

#[test]
fn backpressure_keeps_the_tx_and_sends_it_next() {
	let (a, b) = (tx(1), tx(2));
	let mut t = tracker("backpressure", vec![a.clone(), b.clone()], vec![], 1);
	t.set_phase(Phase::Ramp, 0).unwrap();
	t.start_step(0, 10.0, 0).unwrap();
	// One transiently full queue: the first tx must not be lost (a nonce gap would strand every
	// later tx and blame the chain).
	assert!(!t.send_next(0), "backed up");
	assert!(t.send_next(20) && t.send_next(40));
	assert!(!t.send_next(60), "both sent, nothing left");
	let queued: Vec<Vec<u8>> = t.sender().bytes.clone();
	assert_eq!(queued, vec![a.bytes.to_vec(), b.bytes.to_vec()], "sent in order, none dropped");
	assert!(t.flood.contains_key(&a.hash) && t.flood.contains_key(&b.hash));
	let st = &t.steps[0];
	assert_eq!((st.sent, st.backpressure_ticks, st.starved_ticks), (2, 1, 1));
}

#[test]
fn a_backed_up_probe_is_not_a_refusal_and_is_sent_on_the_next_block() {
	let mut t = tracker("probe-backpressure", vec![], vec![tx(9), tx(10)], 1);
	t.on_block(BlockEvent::Head { number: 2, seen_at: 200 }, 200).unwrap();
	assert!(t.probes.is_empty(), "not sent, so not a probe");
	t.on_block(BlockEvent::Head { number: 3, seen_at: 300 }, 300).unwrap();
	assert_eq!(t.probes.len(), 1);
	assert_eq!(t.probes[0].outcome, Outcome::Pending);
	t.on_block(fetched(4, 400, &[&tx(9)]), 400).unwrap();
	assert_eq!(t.probes[0].outcome, Outcome::Included, "the same probe tx was sent");
}

/// PolkaMeter is paused from 10 s to 18 s. The runner sees it at its first tick after, which may
/// come before or after the blocks read late: their txs are left out of the latency either way.
#[test]
fn a_pause_leaves_out_the_latency_timed_across_it_whichever_is_seen_first() {
	for block_first in [true, false] {
		let (a, b, c) = (tx(1), tx(2), tx(3));
		let mut t = tracker(
			&format!("paused-{block_first}"),
			vec![a.clone(), b.clone(), c.clone()],
			vec![],
			0,
		);
		t.set_phase(Phase::Ramp, 0).unwrap();
		t.start_step(0, 1.0, 0).unwrap();
		assert!(t.send_next(0));
		block(&mut t, 1, 6_000, Some(6_000), &[&a]);
		assert!(t.send_next(8_000) && t.send_next(9_000), "in flight when the pause starts");
		if block_first {
			block(&mut t, 2, 18_000, None, &[&b]);
			t.paused(10_000, 18_000);
		} else {
			t.paused(10_000, 18_000);
			block(&mut t, 2, 18_000, None, &[&b]);
		}
		block(&mut t, 3, 24_000, None, &[&c]);
		let st = &t.steps[0];
		assert_eq!((st.included, st.untimed, st.latencies_ms.clone()), (3, 2, vec![6_000]));
	}
}

fn head(number: u32, seen_at: u64) -> BlockEvent {
	BlockEvent::Head { number, seen_at }
}

/// A block as the follower reads it: its head, then the fetched block with its gap.
fn block(t: &mut Tracker<FakeSender>, number: u32, seen_at: u64, gap: Option<u64>, txs: &[&Tx]) {
	t.on_block(head(number, seen_at), seen_at).unwrap();
	let record = BlockRecord { number, seen_at, gap_ms: gap, ..Default::default() };
	let txs = txs.iter().map(|t| (t.hash, false)).collect();
	t.on_block(BlockEvent::Fetched { record, txs }, seen_at).unwrap();
}

#[test]
fn a_dropped_sender_connection_resends_and_is_not_node_down() {
	let mut t = tracker("interrupted", vec![tx(1), tx(2), tx(3)], vec![], 0);
	t.set_phase(Phase::Ramp, 0).unwrap();
	t.start_step(0, 1.0, 0).unwrap();
	assert!(t.send_next(0) && t.send_next(0) && t.send_next(0));
	assert_eq!(t.inflight(), 3);
	// Requests 1 and 2 were written and not answered; 3 still waits in the queue.
	t.on_reply(Reply::Interrupted { connection: 0, ids: vec![1, 2], reason: "reset".into() });
	assert_eq!(t.inflight(), 3, "sent again, not given up");
	assert_eq!(t.closed_by_node, 0);
	assert_eq!(t.oldest_pending_ms(10_000), 0, "nothing waits on the node while it is down");
	let from = t.observation_gaps[0].from;
	t.on_reply(Reply::Reconnected { connection: 0, down_ms: 500 });
	assert_eq!(t.oldest_pending_ms(from + 1_500), 1_000, "they wait from the reconnect");
	// The node had read request 1 before the drop: "already imported" means it has it.
	let already = |id| Reply::Refused {
		id,
		at: from + 600,
		code: 1013,
		error: "Transaction Already Imported".into(),
	};
	t.on_reply(already(1));
	t.on_reply(Reply::Accepted { id: 2, at: from + 700 });
	t.on_reply(Reply::Accepted { id: 3, at: from + 700 });
	assert!(t.flood[&tx(1).hash].accepted && !t.flood[&tx(1).hash].refused);
	assert_eq!(t.steps[0].rejected, 0);
	assert_eq!(t.steps[0].reply_ms, vec![100, 200, 200], "from the reconnect");
	let gap = &t.observation_gaps[0];
	assert_eq!(
		(gap.what.as_str(), gap.to.map(|to| to - gap.from)),
		("sender connection 0", Some(500))
	);
}

#[test]
fn a_refusal_is_redacted_whole_before_it_is_cut_to_its_error_key() {
	let mut t = tracker("redacted-refusal", vec![tx(1)], vec![], 0);
	t.redact_with(polkameter::redact::Redactor::new(["SeedPhraseSensitive"], None::<&str>));
	t.set_phase(Phase::Ramp, 0).unwrap();
	t.start_step(0, 1.0, 0).unwrap();
	assert!(t.send_next(0));
	// Cut first, the key would end in `SeedPhraseSen`, which no later pass recognizes.
	let error = format!("{} SeedPhraseSensitive", "z".repeat(145));
	t.on_reply(Reply::Refused { id: 1, at: 10, code: 1010, error });
	let keys: Vec<&String> = t.steps[0].errors.keys().collect();
	// The cut falls inside `[redacted]`, which is dropped rather than shown in part.
	assert_eq!(keys, [&format!("1010 {}", "z".repeat(145))]);
}

fn live(t: &Tracker<FakeSender>, now: u64) -> polkameter::load::rules::LiveState {
	polkameter::load::rules::LiveState {
		now,
		connections: 1,
		last_block_at: t.last_head.0,
		last_finalized_at: t.last_finalized.0,
		unobserved_since_block: t.unobserved_between(t.last_head.0, now),
		unobserved_since_finalized: t.unobserved_between(t.last_finalized.0, now),
		node_down: t.node_down.clone(),
		..Default::default()
	}
}

fn rule(t: &Tracker<FakeSender>, now: u64) -> Option<Rule> {
	polkameter::load::rules::live_failure(&live(t, now), &polkameter::load::rules::RULES)
		.map(|s| s.rule)
}

fn drop_and_reconnect(t: &mut Tracker<FakeSender>, at: u64, down: u64) {
	let lost = BlockEvent::Disconnected { at, reason: "stream ended".into() };
	t.on_block(lost, at).unwrap();
	t.on_block(BlockEvent::Reconnected { at: at + down }, at + down).unwrap();
}

/// A tracker in its first ramp step that saw block 1 finalized at time 0.
fn observing(name: &str) -> Tracker<FakeSender> {
	let mut t = tracker(name, vec![tx(1)], vec![], 0);
	t.set_phase(Phase::Ramp, 0).unwrap();
	t.start_step(0, 1.0, 0).unwrap();
	t.on_block(BlockEvent::Finalized { number: 1, seen_at: 0 }, 0).unwrap();
	block(&mut t, 1, 0, None, &[]);
	t
}

/// Time not observed is left out of the stall clocks and never restarts them, whichever node
/// answers after a reconnect; a node that stays unreachable is down.
#[test]
fn reconnects_never_hide_a_stall() {
	// A drop-and-reconnect every 10 s, with best blocks (finality stall) or without (stall).
	for (best_blocks, down, expected) in
		[(true, 1_000, (Rule::FinalityStall, 69_000)), (false, 2_000, (Rule::Stall, 39_000))]
	{
		let mut t = observing("reconnects");
		let mut stopped = None;
		for cycle in 0..12u32 {
			let at = u64::from(cycle) * 10_000;
			if best_blocks {
				block(&mut t, 2 + cycle, at + 1_000, Some(6_000), &[]);
			}
			drop_and_reconnect(&mut t, at + 5_000, down);
			if let Some(rule) = rule(&t, at + 9_000) {
				stopped = Some((rule, at + 9_000));
				break;
			}
		}
		assert_eq!(stopped, Some(expected));
	}

	// A reconnect to another node (load balancer) is one warning, not node down; a dead chain
	// still stalls, then stalls finality.
	let mut t = observing("other-node");
	drop_and_reconnect(&mut t, 2_000, 1_000);
	for _ in 0..2 {
		t.on_block(BlockEvent::OtherNode("its peer ID changed".into()), 3_000).unwrap();
	}
	assert_eq!((t.node_down.clone(), t.warnings.len()), (None, 1));
	assert_eq!(rule(&t, 30_000), None, "29 s observed");
	assert_eq!(rule(&t, 33_000), Some(Rule::Stall), "32 s observed");
	block(&mut t, 2, 32_000, None, &[]);
	block(&mut t, 3, 50_000, None, &[]);
	assert_eq!(rule(&t, 63_000), Some(Rule::FinalityStall), "62 s observed after finalized 1");

	// A long follower outage: only observed time counts, and the gap spans the outage.
	let mut t = observing("follower-gap");
	drop_and_reconnect(&mut t, 2_000, 38_000);
	assert_eq!(rule(&t, 50_000), None, "12 s observed");
	assert_eq!(rule(&t, 70_000), Some(Rule::Stall), "32 s observed, across the reconnect");
	block(&mut t, 9, 41_000, None, &[]);
	t.end_step(60_000);
	assert_eq!((t.steps[0].unobserved_ms, t.steps[0].max_gap_ms), (38_000, Some(19_000)));
	t.on_block(BlockEvent::Unreachable("not reachable for 15 s".into()), 61_000)
		.unwrap();
	assert_eq!(rule(&t, 61_000), Some(Rule::NodeDown));
}
