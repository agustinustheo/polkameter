//! The tracker as a state machine: a fake sender, hand-made replies and block events, no network.

use polkameter::load::{
	Lane, QueueSource, Tx,
	follower::BlockEvent,
	sender::Reply,
	submit::Submit,
	tracker::{Phase, Tracker},
};
use polkameter_plugin_sdk::observers::files::{
	BlockRecord, RunDir,
	summary::{Outcome, Rule},
};

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
	let stop =
		rules::check_step(&t.steps[0], &t.lanes()[0].source, 0, 6_000.0, &rules::RULES, false);
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
fn a_dropped_sender_connection_resends_and_is_not_node_down() {
	let mut t = tracker("interrupted", vec![tx(1), tx(2), tx(3)], vec![]);
	t.set_phase(Phase::Ramp, 0).unwrap();
	t.start_step(0, &[1.0], 0).unwrap();
	assert!(t.send_next(0, 0) && t.send_next(0, 0) && t.send_next(0, 0));
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
	assert_eq!(t.steps[0][0].rejected, 0);
	assert_eq!(t.steps[0][0].reply_ms, vec![100, 200, 200], "from the reconnect");
	let gap = &t.observation_gaps[0];
	assert_eq!(
		(gap.what.as_str(), gap.to.map(|to| to - gap.from)),
		("sender connection 0", Some(500))
	);
}

#[test]
fn already_imported_is_a_refusal_for_a_first_submit() {
	let mut t = tracker("first-refusal", vec![tx(1)], vec![]);
	t.set_phase(Phase::Ramp, 0).unwrap();
	t.start_step(0, &[1.0], 0).unwrap();
	assert!(t.send_next(0, 0));
	t.on_reply(Reply::Refused { id: 1, at: 10, code: 1013, error: "Already Imported".into() });
	assert_eq!(t.steps[0][0].rejected, 1, "unchanged for a tx sent once");
}

#[test]
fn a_refusal_is_redacted_whole_before_it_is_cut_to_its_error_key() {
	let mut t = tracker("redacted-refusal", vec![tx(1)], vec![]);
	t.redact_with(polkameter::redact::Redactor::new(["SeedPhraseSensitive"], None::<&str>));
	t.set_phase(Phase::Ramp, 0).unwrap();
	t.start_step(0, &[1.0], 0).unwrap();
	assert!(t.send_next(0, 0));
	// Cut first, the key would end in `SeedPhraseSen`, which no later pass recognizes.
	let error = format!("{} SeedPhraseSensitive", "z".repeat(145));
	t.on_reply(Reply::Refused { id: 1, at: 10, code: 1010, error });
	let keys: Vec<&String> = t.steps[0][0].errors.keys().collect();
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

#[test]
fn a_follower_reconnect_leaves_out_only_the_time_not_observed() {
	let mut t = tracker("follower-gap", vec![tx(1)], vec![]);
	t.set_phase(Phase::Ramp, 0).unwrap();
	t.start_step(0, &[1.0], 0).unwrap();
	t.on_block(BlockEvent::Finalized { number: 1, seen_at: 1_000 }, 1_000).unwrap();
	block(&mut t, 1, 1_000, None, &[]);
	let lost = BlockEvent::Disconnected { at: 2_000, reason: "stream ended".into() };
	t.on_block(lost, 2_000).unwrap();
	assert!(!t.observing());
	assert_eq!(rule(&t, 40_000), None, "1 s observed, 38 s not");
	t.on_block(BlockEvent::Reconnected { at: 40_000 }, 40_000).unwrap();
	assert!(t.observing());
	assert_eq!(rule(&t, 50_000), None, "11 s observed");
	assert_eq!(rule(&t, 70_000), Some(Rule::Stall), "31 s observed, across the reconnect");
	block(&mut t, 9, 41_000, None, &[]);
	t.end_step(60_000);
	let st = &t.steps[0][0];
	assert_eq!(st.unobserved_ms, 38_000);
	assert_eq!(st.max_gap_ms, Some(19_000), "the gaps around the outage, not the outage");
	assert!(t.observation_gaps[0].to_string().contains("reconnected after 38.0 s"));
	// A node that stayed unreachable is down.
	t.on_block(BlockEvent::Unreachable("not reachable for 15 s".into()), 61_000)
		.unwrap();
	assert_eq!(rule(&t, 61_000), Some(Rule::NodeDown));
}

/// Behind a load balancer a reconnect can land on another, healthy node: that is a warning, not
/// node down, and the load goes on while blocks come. A chain that is really dead still stops
/// on a stall, whichever node answers.
#[test]
fn a_reconnect_to_another_node_is_a_warning_and_a_dead_chain_still_stalls() {
	let mut t = tracker("other-node", vec![tx(1)], vec![]);
	t.set_phase(Phase::Ramp, 0).unwrap();
	t.start_step(0, &[1.0], 0).unwrap();
	t.on_block(BlockEvent::Finalized { number: 1, seen_at: 1_000 }, 1_000).unwrap();
	block(&mut t, 1, 1_000, None, &[]);
	let why = "reconnected to a different node (load balancer, or the node restarted): its peer ID changed from a to b";
	t.on_block(BlockEvent::Disconnected { at: 2_000, reason: "stream ended".into() }, 2_000)
		.unwrap();
	t.on_block(BlockEvent::OtherNode(why.into()), 3_000).unwrap();
	t.on_block(BlockEvent::Reconnected { at: 3_000 }, 3_000).unwrap();
	assert_eq!(t.node_down, None);
	assert_eq!(t.warnings, [why]);
	// The same node again on a later reconnect is not listed twice.
	t.on_block(BlockEvent::OtherNode(why.into()), 3_000).unwrap();
	assert_eq!(t.warnings.len(), 1);
	assert_eq!(rule(&t, 4_000), None, "the load goes on");
	// No block after the reconnect: 30 s observed without one is a stall.
	assert_eq!(rule(&t, 31_000), None, "29 s observed");
	assert_eq!(rule(&t, 33_000), Some(Rule::Stall), "31 s observed");
	// Blocks come, finality does not: a finality stall.
	block(&mut t, 2, 32_000, None, &[]);
	block(&mut t, 3, 50_000, None, &[]);
	assert_eq!(rule(&t, 63_000), Some(Rule::FinalityStall), "61 s observed after finalized 1");
}

/// Best blocks keep coming, finality does not, and the follower reconnects every 10 s: the
/// reconnects must not restart the finality stall clock.
#[test]
fn reconnects_do_not_hide_a_finality_stall() {
	let mut t = tracker("finality-reconnects", vec![tx(1)], vec![]);
	t.set_phase(Phase::Ramp, 0).unwrap();
	t.start_step(0, &[1.0], 0).unwrap();
	t.on_block(BlockEvent::Finalized { number: 1, seen_at: 0 }, 0).unwrap();
	let mut stopped = None;
	for cycle in 0..12u32 {
		let at = u64::from(cycle) * 10_000;
		block(&mut t, 2 + cycle, at + 1_000, Some(6_000), &[]);
		drop_and_reconnect(&mut t, at + 5_000, 1_000);
		if let Some(rule) = rule(&t, at + 9_000) {
			stopped = Some((rule, at + 9_000));
			break;
		}
	}
	// 60 s of observed time without a new finalized block: 66 s with 6 reconnects of 1 s.
	assert_eq!(stopped, Some((Rule::FinalityStall, 69_000)));
}

/// Six drop-and-reconnect cycles in 60 s and not one block: a stall, not six fresh starts.
#[test]
fn reconnects_do_not_hide_a_stall() {
	let mut t = tracker("stall-reconnects", vec![tx(1)], vec![]);
	t.set_phase(Phase::Ramp, 0).unwrap();
	t.start_step(0, &[1.0], 0).unwrap();
	block(&mut t, 1, 0, None, &[]);
	t.on_block(BlockEvent::Finalized { number: 1, seen_at: 0 }, 0).unwrap();
	let mut stopped = None;
	for cycle in 0..6u64 {
		drop_and_reconnect(&mut t, cycle * 10_000 + 8_000, 2_000);
		let now = cycle * 10_000 + 10_000;
		if let Some(rule) = rule(&t, now) {
			stopped = Some((rule, now));
			break;
		}
	}
	// 8 s observed per 10 s cycle: over 30 s observed after the fourth cycle.
	assert_eq!(stopped, Some((Rule::Stall, 40_000)));
}
