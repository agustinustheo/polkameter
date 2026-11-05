//! The load loop on tokio's paused clock: steps and the token bucket run on monotonic time, so a
//! wall clock that jumps changes nothing.

use std::sync::atomic::{AtomicU64, Ordering};

use polkameter::load::{
	Lane, Plan, QueueSource, Tx,
	rules::RULES,
	runner::{Io, Mode, RunOptions, load},
	sender::Reply,
	steps::final_step,
	submit::Submit,
	tracker::{Phase, Tracker},
};
use polkameter_plugin_sdk::observers::files::{Problems, RunDir, now_ms, summary::Rule};

/// A node that accepts every tx at once.
struct FakeSender(tokio::sync::mpsc::UnboundedSender<Reply>);

impl Submit for FakeSender {
	fn submit(&mut self, id: u64, _bytes: &[u8]) -> Option<usize> {
		self.0.send(Reply::Accepted { id, at: now_ms() }).ok()?;
		Some(0)
	}
	fn queued_bytes(&self) -> u64 {
		0
	}
}

static WALL: AtomicU64 = AtomicU64::new(1_000_000_000_000_000);

/// A wall clock that steps a minute back every time it is read.
fn wall_going_back() -> u64 {
	WALL.fetch_sub(60_000, Ordering::Relaxed)
}

#[tokio::test(start_paused = true)]
async fn steps_run_on_monotonic_time_whatever_the_wall_clock_does() {
	let dir =
		RunDir::create(std::path::Path::new(env!("CARGO_TARGET_TMPDIR")), "monotonic").unwrap();
	let txs: Vec<Tx> = (0..200u32).map(|n| Tx::new(n.to_le_bytes().to_vec())).collect();
	let lane = Lane { call: "Test.call", source: QueueSource::new(txs, vec![], "txs") };
	let (replies_tx, replies) = tokio::sync::mpsc::unbounded_channel();
	let mut t = Tracker::new(
		vec![lane],
		FakeSender(replies_tx),
		dir.jsonl("load.jsonl").unwrap(),
		dir.jsonl("blocks.jsonl").unwrap(),
		0,
		now_ms(),
	);
	t.set_phase(Phase::Ramp, now_ms()).unwrap();
	// Keep the stall rules quiet: a block and a finalized block arrive "now" as the step starts.
	t.last_head.0 = now_ms() + 60_000;
	t.last_finalized.0 = now_ms() + 60_000;
	let (_blocks_tx, blocks) = tokio::sync::mpsc::unbounded_channel();
	let (events, _) = tokio::sync::broadcast::channel(64);
	let mut io = Io {
		replies,
		blocks,
		events,
		problems: Problems::default(),
		tool_failed: tokio_util::sync::CancellationToken::new(),
		wall: wall_going_back,
	};
	let opts = RunOptions {
		rules: RULES,
		mode: Mode::Stress,
		plan: Plan::ramp(2.0, 1.0, 20, 2),
		connections: 1,
		recovery_s: 10,
		baseline_probes: 0,
		block_interval_s: 6.0,
	};
	let real = std::time::Instant::now();
	let virtual_start = tokio::time::Instant::now();
	let stop = load(&mut t, &mut io, &opts).await.unwrap();
	// 40 s of steps on the paused clock, in far less real time: the steps end by the monotonic
	// clock, not the wall clock.
	assert!(real.elapsed() < std::time::Duration::from_secs(10), "{:?}", real.elapsed());
	assert_eq!((stop.rule, stop.step), (Rule::RateCap, Some(1)), "{}", stop.detail);
	assert_eq!(virtual_start.elapsed().as_secs(), 40);
	let steps: Vec<_> = t.steps[0].iter().map(|s| final_step(s, None)).collect();
	assert_eq!(steps.iter().map(|f| f.seconds).collect::<Vec<_>>(), [20.0, 20.0]);
	// 2 then 3 tx/s; the bucket starts empty, so the first step sends one fewer at most.
	assert!((39..=40).contains(&steps[0].sent), "{}", steps[0].sent);
	assert!((59..=60).contains(&steps[1].sent), "{}", steps[1].sent);
}
