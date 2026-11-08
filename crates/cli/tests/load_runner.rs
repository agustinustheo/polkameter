//! The load loop on tokio's paused clock: steps and the token bucket run on monotonic time, and
//! only a clock that counts sleep can show a suspended machine.

use std::sync::atomic::{AtomicU64, Ordering};

use polkameter::load::{
	Lane, Plan, QueueSource, Tx,
	rules::RULES,
	runner::{Io, Mode, RunOptions, load, recover},
	sender::Reply,
	steps::final_step,
	submit::Submit,
	tracker::{Phase, Tracker},
};
use polkameter_plugin_sdk::observers::{Problems, RunDir, now_ms, summary::Rule};

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

/// A clock that counts sleep and, here, follows the paused clock: no suspend.
fn boot_awake() -> Option<u64> {
	Some(now_ms())
}

static SLEPT: AtomicU64 = AtomicU64::new(0);

/// The same, but the machine is suspended for a minute 5 s into the run.
fn boot_suspended() -> Option<u64> {
	static START: std::sync::OnceLock<u64> = std::sync::OnceLock::new();
	let now = now_ms();
	let start = *START.get_or_init(|| now);
	if now >= start + 5_000 {
		SLEPT.store(60_000, Ordering::Relaxed);
	}
	Some(now + SLEPT.load(Ordering::Relaxed))
}

/// A tracker in the load, a healthy chain (a new best block every 6 s) and the runner's inputs.
fn setup(
	name: &str,
	boot: fn() -> Option<u64>,
	restarts: Problems,
) -> (Tracker<FakeSender>, Io, RunOptions) {
	setup_with(name, boot, restarts, 200)
}

/// [`setup`] with a flood of `flood` txs.
fn setup_with(
	name: &str,
	boot: fn() -> Option<u64>,
	restarts: Problems,
	flood: u32,
) -> (Tracker<FakeSender>, Io, RunOptions) {
	let dir = RunDir::create(std::path::Path::new(env!("CARGO_TARGET_TMPDIR")), name).unwrap();
	let txs: Vec<Tx> = (0..flood).map(|n| Tx::new(n.to_le_bytes().to_vec())).collect();
	let lane = Lane { call: "Test.call", source: QueueSource::new(txs, vec![], "txs") };
	let (replies_tx, replies) = tokio::sync::mpsc::unbounded_channel();
	let mut t = Tracker::new(
		lane,
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
	let (blocks_tx, blocks) = tokio::sync::mpsc::unbounded_channel();
	// A healthy chain: a new best block every 6 s.
	tokio::spawn(async move {
		for number in 1.. {
			tokio::time::sleep(std::time::Duration::from_secs(6)).await;
			let head = polkameter::load::follower::BlockEvent::Head { number, seen_at: now_ms() };
			if blocks_tx.send(head).is_err() {
				return;
			}
		}
	});
	let (events, _) = tokio::sync::broadcast::channel(64);
	let io = Io {
		replies,
		blocks,
		events,
		problems: Problems::default(),
		tool_failed: tokio_util::sync::CancellationToken::new(),
		boot,
		restarts,
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
	(t, io, opts)
}

async fn run(
	name: &str,
	boot: fn() -> Option<u64>,
	restarts: Problems,
) -> (Tracker<FakeSender>, polkameter_plugin_sdk::observers::summary::Stop, std::time::Duration) {
	let (mut t, mut io, opts) = setup(name, boot, restarts);
	let virtual_start = tokio::time::Instant::now();
	let stop = load(&mut t, &mut io, &opts).await.unwrap();
	(t, stop, virtual_start.elapsed())
}

#[tokio::test(start_paused = true)]
async fn steps_run_on_monotonic_time() {
	let real = std::time::Instant::now();
	let (t, stop, elapsed) = run("monotonic", boot_awake, Problems::default()).await;
	// 40 s of steps on the paused clock, in far less real time: the steps end by the monotonic
	// clock, not the wall clock (which is never read).
	assert!(real.elapsed() < std::time::Duration::from_secs(10), "{:?}", real.elapsed());
	assert_eq!((stop.rule, stop.step), (Rule::RateCap, Some(1)), "{}", stop.detail);
	assert_eq!(elapsed.as_secs(), 40);
	let steps: Vec<_> = t.steps.iter().map(final_step).collect();
	assert_eq!(steps.iter().map(|f| f.seconds).collect::<Vec<_>>(), [20.0, 20.0]);
	// 2 then 3 tx/s; the bucket starts empty, so the first step sends one fewer at most.
	assert!((39..=40).contains(&steps[0].sent), "{}", steps[0].sent);
	assert!((59..=60).contains(&steps[1].sent), "{}", steps[1].sent);
}

/// A flood with exactly the txs the plan sends is empty after the last step: every step ran, so
/// the load ends on the rate cap, not on budget used up (which would make the verdict
/// INCONCLUSIVE). A flood that runs out during the last step is still budget used up.
#[tokio::test(start_paused = true)]
async fn a_flood_sized_to_the_plan_reaches_the_rate_cap() {
	// What the plan sends on the paused clock, from a flood larger than it needs.
	let (t, stop, _) = run("sized-probe", boot_awake, Problems::default()).await;
	assert_eq!(stop.rule, Rule::RateCap, "{}", stop.detail);
	let sent: u64 = t.steps.iter().map(|s| final_step(s).sent).sum();
	assert!((99..=100).contains(&sent), "2 tx/s for 20 s, then 3 tx/s for 20 s: {sent}");

	let (mut t, mut io, opts) =
		setup_with("sized", boot_awake, Problems::default(), u32::try_from(sent).unwrap());
	let stop = load(&mut t, &mut io, &opts).await.unwrap();
	assert!(t.lane().source.exhausted(), "the flood was sized to the plan");
	assert_eq!((stop.rule, stop.step), (Rule::RateCap, Some(1)), "{}", stop.detail);

	let (mut t, mut io, opts) = setup_with("short", boot_awake, Problems::default(), 50);
	let stop = load(&mut t, &mut io, &opts).await.unwrap();
	assert_eq!((stop.rule, stop.step), (Rule::BudgetUsedUp, Some(1)), "{}", stop.detail);
}

#[tokio::test(start_paused = true)]
async fn a_suspended_machine_stops_the_load_as_a_generator_limit() {
	let (_, stop, _) = run("suspended", boot_suspended, Problems::default()).await;
	assert_eq!((stop.rule, stop.step), (Rule::GeneratorLimit, Some(0)), "{}", stop.detail);
}

/// The process sampler saw the node's PID change: the node restarted. That is node down in the
/// load even though every connection reconnected, and it ends recovery early instead of waiting
/// out its time.
#[tokio::test(start_paused = true)]
async fn a_node_restart_the_sampler_saw_is_node_down_in_load_and_recovery() {
	const RESTARTED: &str = "node: process 10 ended; sampling the node process now on its port, 20";
	let restarts = Problems::default();
	restarts.record(RESTARTED.into());
	let (_, stop, _) = run("restarted", boot_awake, restarts).await;
	assert_eq!((stop.rule, stop.step), (Rule::NodeDown, Some(0)), "{}", stop.detail);

	let restarts = Problems::default();
	let (mut t, mut io, mut opts) = setup("restarted-in-recovery", boot_awake, restarts.clone());
	opts.recovery_s = 120;
	tokio::spawn(async move {
		tokio::time::sleep(std::time::Duration::from_secs(20)).await;
		restarts.record(RESTARTED.into());
	});
	let started = tokio::time::Instant::now();
	let r = recover(&mut t, &mut io, &opts, 12_000).await.unwrap();
	assert!(started.elapsed() < std::time::Duration::from_secs(21), "{:?}", started.elapsed());
	assert!(!r.recovered, "{}", r.detail);
}
