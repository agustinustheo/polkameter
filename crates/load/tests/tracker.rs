//! The tracker as a state machine: a fake sender, hand-made replies and block events, no network.

use polkameter_files::summary::Outcome;
use polkameter_files::{BlockRecord, RunDir};
use polkameter_load::follower::BlockEvent;
use polkameter_load::sender::Reply;
use polkameter_load::submit::Submit;
use polkameter_load::tracker::{Phase, Tracker};
use polkameter_load::{Lane, QueueSource, Tx};

#[derive(Default)]
struct FakeSender {
	sent: Vec<u64>,
}

impl Submit for FakeSender {
	fn submit(&mut self, id: u64, _bytes: &[u8]) -> Option<usize> {
		self.sent.push(id);
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
	let lane = Lane { call: "Test.call", source: Box::new(QueueSource::new(flood, probes, "txs")) };
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
	BlockEvent::Fetched {
		record,
		hash: [number as u8; 32],
		txs: txs.iter().map(|t| (t.hash, false)).collect(),
	}
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
	t.on_block(BlockEvent::Head { number: 2, hash: [2; 32], seen_at: 200 }, 200)
		.unwrap();
	assert_eq!(t.probes.len(), 1);
	t.on_block(BlockEvent::Head { number: 3, hash: [3; 32], seen_at: 300 }, 300)
		.unwrap();
	assert_eq!(t.probes.len(), 1, "only baseline_probes = 1");
	t.on_reply(Reply::Closed { connection: 0, by_node: true });
	assert_eq!(t.probes[0].outcome, Outcome::Pending);
	t.on_block(fetched(4, 400, &[&tx(9)]), 400).unwrap();
	assert_eq!(t.probes[0].outcome, Outcome::Included);
	assert_eq!(t.closed_by_node, 1);
}
