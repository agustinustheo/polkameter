//! The tracker as a state machine: a fake sender, hand-made replies and block events, no network.

use polkameter::load::{
	Lane, QueueSource, Tx,
	follower::BlockEvent,
	sender::Reply,
	submit::Submit,
	tracker::{Phase, Tracker},
};
use polkameter_plugin_sdk::observers::files::{BlockRecord, RunDir, summary::Outcome};

#[derive(Default)]
struct FakeSender {
	sent: Vec<u64>,
	/// The bytes queued, in order.
	bytes: Vec<Vec<u8>>,
	/// Submits still to refuse as backed up.
	full: usize,
}

impl Submit for FakeSender {
	fn submit(&mut self, id: u64, bytes: &[u8]) -> Option<usize> {
		if self.full > 0 {
			self.full -= 1;
			return None;
		}
		self.sent.push(id);
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

fn tracker(name: &str, flood: Vec<Tx>, probes: Vec<Tx>) -> Tracker<FakeSender> {
	let dir = RunDir::create(std::path::Path::new(env!("CARGO_TARGET_TMPDIR")), name).unwrap();
	let lane = Lane { call: "Test.call", source: QueueSource::new(flood, probes, "txs") };
	Tracker::new(
		vec![lane],
		FakeSender::default(),
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
	let mut t = tracker("late-reply", vec![a.clone(), b.clone()], vec![]);
	t.set_phase(Phase::Ramp, 1_000).unwrap();
	t.start_step(0, &[10.0], 1_000).unwrap();
	assert!(t.send_next(0, 1_000) && t.send_next(0, 1_000));
	t.on_block(fetched(5, 1_500, &[&a, &b]), 1_500).unwrap();
	t.on_reply(Reply::Accepted { id: 1, at: 1_600 });
	t.on_reply(Reply::Refused {
		id: 2,
		at: 1_700,
		code: 1014,
		error: "Priority is too low".into(),
	});
	let st = &t.steps[0][0];
	assert_eq!((st.included, st.rejected), (2, 0), "included twice, the late refusal ignored");
	assert!(!t.flood[&b.hash].refused, "a late refusal does not flag an included tx");
	assert!(t.flood[&a.hash].accepted, "the accepted reply counts");
	assert_eq!(st.reply_ms, vec![600, 700], "both reply times counted");
	assert_eq!(st.blocks.len(), 1);
	assert_eq!(st.blocks[0].ours, 2);
}

#[test]
fn blocks_go_to_the_step_they_were_seen_in() {
	let (a, b) = (tx(1), tx(2));
	let mut t = tracker("by-seen-at", vec![a.clone(), b.clone()], vec![]);
	t.set_phase(Phase::Ramp, 0).unwrap();
	t.start_step(0, &[1.0], 0).unwrap();
	assert!(t.send_next(0, 0));
	t.end_step(10_000);
	t.start_step(1, &[1.0], 10_000).unwrap();
	// A block seen in step 0 arrives after step 1 started (a slow fetch).
	t.on_block(fetched(7, 9_000, &[&a]), 11_000).unwrap();
	assert_eq!(t.steps[0][0].blocks.len(), 1);
	assert!(t.steps[0][1].blocks.is_empty());
}

#[test]
fn an_empty_source_counts_a_starved_tick() {
	let mut t = tracker("starved", vec![tx(1)], vec![]);
	t.set_phase(Phase::Ramp, 0).unwrap();
	t.start_step(0, &[1.0], 0).unwrap();
	assert!(t.send_next(0, 0));
	assert!(!t.send_next(0, 0));
	assert_eq!(t.steps[0][0].starved_ticks, 1);
}

#[test]
fn closed_connection_preserves_unknown_probe_until_observed() {
	let mut t = tracker("probes", vec![], vec![tx(9), tx(10)]);
	t.on_block(fetched(1, 100, &[]), 100).unwrap();
	assert!(t.probes.is_empty(), "a fetched block sends no probe");
	t.on_block(BlockEvent::Head { number: 2, seen_at: 200 }, 200).unwrap();
	assert_eq!(t.probes.len(), 1);
	t.on_block(BlockEvent::Head { number: 3, seen_at: 300 }, 300).unwrap();
	assert_eq!(t.probes.len(), 1, "only baseline_probes = 1");
	t.on_reply(Reply::Closed { connection: 0, by_node: true });
	assert_eq!(t.probes[0].outcome, Outcome::Pending);
	t.on_block(fetched(4, 400, &[&tx(9)]), 400).unwrap();
	assert_eq!(t.probes[0].outcome, Outcome::Included);
	assert_eq!(t.closed_by_node, 1);
}

#[test]
fn backpressure_keeps_the_tx_and_sends_it_next() {
	let (a, b) = (tx(1), tx(2));
	let mut t = tracker("backpressure", vec![a.clone(), b.clone()], vec![]);
	t.set_phase(Phase::Ramp, 0).unwrap();
	t.start_step(0, &[10.0], 0).unwrap();
	// One transiently full queue: the first tx must not be lost (a nonce gap would strand every
	// later tx and blame the chain).
	t.sender_mut().full = 1;
	assert!(!t.send_next(0, 0), "backed up");
	assert!(t.send_next(0, 20) && t.send_next(0, 40));
	assert!(!t.send_next(0, 60), "both sent, nothing left");
	let queued: Vec<Vec<u8>> = t.sender().bytes.clone();
	assert_eq!(queued, vec![a.bytes.to_vec(), b.bytes.to_vec()], "sent in order, none dropped");
	assert!(t.flood.contains_key(&a.hash) && t.flood.contains_key(&b.hash));
	let st = &t.steps[0][0];
	assert_eq!((st.sent, st.backpressure_ticks, st.starved_ticks), (2, 1, 1));
}

#[test]
fn a_backed_up_probe_is_not_a_refusal_and_is_sent_on_the_next_block() {
	let mut t = tracker("probe-backpressure", vec![], vec![tx(9), tx(10)]);
	t.sender_mut().full = 1;
	t.on_block(BlockEvent::Head { number: 2, seen_at: 200 }, 200).unwrap();
	assert!(t.probes.is_empty(), "not sent, so not a probe");
	t.on_block(BlockEvent::Head { number: 3, seen_at: 300 }, 300).unwrap();
	assert_eq!(t.probes.len(), 1);
	assert_eq!(t.probes[0].outcome, Outcome::Pending);
	t.on_block(fetched(4, 400, &[&tx(9)]), 400).unwrap();
	assert_eq!(t.probes[0].outcome, Outcome::Included, "the same probe tx was sent");
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
fn a_block_gap_counts_in_the_steps_it_spans() {
	use polkameter::load::{rules, steps::final_step};
	use polkameter_plugin_sdk::observers::files::summary::Rule;
	let mut t = tracker("gap-edges", (1..=60).map(tx).collect(), vec![]);
	t.set_phase(Phase::Ramp, 0).unwrap();
	block(&mut t, 1, 0, None, &[]);
	t.start_step(0, &[1.0], 0).unwrap();
	block(&mut t, 2, 6_000, Some(6_000), &[]);
	// The chain stalls from 6 s; step 1 starts at 30 s; the next block comes at 34 s.
	t.end_step(30_000);
	let f0 = final_step(&t.steps[0][0], None);
	assert_eq!(f0.blocks.max_block_gap_ms, Some(24_000), "the stall in step 0 is step 0's");
	assert_eq!(f0.blocks.mean_block_gap_ms, Some(30_000.0), "one block in 30 s");
	let stop = rules::check_step(&t.steps[0], &t.lanes()[0].source, 0, 6_000.0, &rules::RULES);
	assert_eq!(stop.map(|s| (s.rule, s.step)), Some((Rule::SlowBlocks, Some(0))));
	t.start_step(1, &[1.0], 30_000).unwrap();
	block(&mut t, 3, 34_000, Some(28_000), &[]);
	for (n, at) in [(4, 40_000), (5, 46_000), (6, 52_000), (7, 58_000)] {
		block(&mut t, n, at, Some(6_000), &[]);
	}
	t.end_step(60_000);
	assert_eq!(t.steps[0][0].max_gap_ms, Some(24_000), "unchanged by the block after it");
	let f1 = final_step(&t.steps[0][1], None);
	assert_eq!(f1.blocks.max_block_gap_ms, Some(6_000), "step 1 has 4 s of the stall");
	assert_eq!(f1.blocks.mean_block_gap_ms, Some(6_000.0));
}

#[test]
fn included_per_second_counts_the_step_own_txs() {
	use polkameter::load::steps::final_step;
	let (a, b) = (tx(1), tx(2));
	let mut t = tracker("own-step", vec![a.clone(), b.clone()], vec![]);
	t.set_phase(Phase::Ramp, 0).unwrap();
	t.start_step(0, &[1.0], 0).unwrap();
	assert!(t.send_next(0, 0) && t.send_next(0, 1_000));
	t.end_step(10_000);
	t.start_step(1, &[0.0], 10_000).unwrap();
	// Step 0's txs land in a block of step 1.
	block(&mut t, 5, 12_000, None, &[&a, &b]);
	t.end_step(20_000);
	let (f0, f1) = (final_step(&t.steps[0][0], None), final_step(&t.steps[0][1], None));
	assert_eq!((f0.included, f0.included_per_s), (2, 0.2));
	assert_eq!((f1.included, f1.included_per_s), (0, 0.0), "not step 0's backlog");
	assert_eq!(f1.blocks.max_ours_per_block, 2, "the block itself is step 1's");
}

#[test]
fn a_dropped_sender_connection_is_not_node_down() {
	let mut t = tracker("interrupted", vec![tx(1), tx(2)], vec![]);
	t.set_phase(Phase::Ramp, 0).unwrap();
	t.start_step(0, &[1.0], 0).unwrap();
	assert!(t.send_next(0, 0) && t.send_next(0, 0));
	assert_eq!(t.inflight(), 2);
	t.on_reply(Reply::Interrupted { connection: 0, ids: vec![1], reason: "reset".into() });
	assert_eq!(t.inflight(), 1, "the written request has an unknown outcome; the queued one waits");
	assert_eq!(t.closed_by_node, 0);
	t.on_reply(Reply::Reconnected { connection: 0, down_ms: 500 });
	t.on_reply(Reply::Accepted { id: 2, at: 700 });
	let gap = &t.observation_gaps[0];
	assert_eq!(
		(gap.what.as_str(), gap.to.map(|to| to - gap.from)),
		("sender connection 0", Some(500))
	);
	// The interrupted tx is still watched for, and counts if a block has it.
	t.on_block(fetched(3, 800, &[&tx(1)]), 800).unwrap();
	assert_eq!(t.steps[0][0].included, 1);
}

#[test]
fn a_follower_reconnect_is_an_observation_gap_not_a_stall() {
	use polkameter::load::rules::{LiveState, RULES, live_failure};
	let mut t = tracker("follower-gap", vec![tx(1)], vec![]);
	t.set_phase(Phase::Ramp, 0).unwrap();
	t.start_step(0, &[1.0], 0).unwrap();
	block(&mut t, 1, 1_000, None, &[]);
	let lost = BlockEvent::Disconnected { at: 2_000, reason: "stream ended".into() };
	t.on_block(lost, 2_000).unwrap();
	assert!(!t.observing());
	let live = |t: &Tracker<FakeSender>, now| LiveState {
		now,
		connections: 1,
		last_block_at: t.last_head.0,
		last_finalized_at: now,
		observing: t.observing(),
		node_unreachable: t.node_unreachable.clone(),
		..Default::default()
	};
	assert_eq!(live_failure(&live(&t, 40_000), &RULES), None, "not observed, so no stall");
	t.on_block(BlockEvent::Reconnected { at: 40_000 }, 40_000).unwrap();
	assert!(t.observing());
	assert_eq!(
		live_failure(&live(&t, 50_000), &RULES),
		None,
		"the stall counts from the reconnect"
	);
	block(&mut t, 9, 41_000, None, &[]);
	t.end_step(60_000);
	let st = &t.steps[0][0];
	assert_eq!(st.unobserved_ms, 38_000);
	assert_eq!(st.max_gap_ms, Some(19_000), "the gaps around the outage, not the outage");
	assert!(t.observation_gaps[0].to_string().contains("reconnected after 38.0 s"));
	// A node that stays unreachable is down.
	t.on_block(BlockEvent::Unreachable("no answer for 15 s".into()), 61_000)
		.unwrap();
	let stop = live_failure(&live(&t, 61_000), &RULES).unwrap();
	assert_eq!(stop.rule, polkameter_plugin_sdk::observers::files::summary::Rule::NodeDown);
}
