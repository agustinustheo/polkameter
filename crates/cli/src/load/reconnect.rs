//! Opening an RPC connection again after it dropped, for the sender and the block follower.
//!
//! Each attempt has its own deadline, the whole recovery has one too (about 15 s), and both race
//! the run's cancellation: a node that accepts TCP and never finishes the WebSocket handshake
//! cannot hold the run. A connection that keeps dropping is bounded as well: more than
//! [`MAX_DROPS`] drops within [`DROP_WINDOW`] count as a node that is down.

use std::{collections::VecDeque, fmt::Display, future::Future, time::Duration};

use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

/// Drops of one connection within [`DROP_WINDOW`] that are still reconnected.
pub const MAX_DROPS: usize = 5;
/// The window [`MAX_DROPS`] counts in.
pub const DROP_WINDOW: Duration = Duration::from_secs(60);

/// How to try again: the wait before each attempt (the last one repeats), the longest one
/// attempt may take, and the longest the whole recovery may take.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Backoff {
	/// Waits before each attempt.
	pub waits: Vec<Duration>,
	/// One attempt's deadline: connecting, the handshake and subscribing.
	pub attempt: Duration,
	/// The deadline of the whole recovery; after it the node is down.
	pub total: Duration,
}

impl Default for Backoff {
	/// 0.25, 0.5, 1, 2, 4 and 8 s between attempts, 5 s per attempt, 15 s in all.
	fn default() -> Self {
		Self {
			waits: [250, 500, 1_000, 2_000, 4_000, 8_000].map(Duration::from_millis).to_vec(),
			attempt: Duration::from_secs(5),
			total: Duration::from_secs(15),
		}
	}
}

/// How a recovery ended.
#[derive(Debug, PartialEq, Eq)]
pub enum Reconnect<T> {
	/// Open again.
	Connected(T),
	/// The run stopped meanwhile.
	Stopped,
	/// Not open by the deadline: the node is down. The last attempt's error.
	GaveUp(String),
}

/// Tries `connect` until it succeeds, `stop` is cancelled or the backoff's deadline passes.
pub async fn reconnect<T, E, F, Fut>(
	mut connect: F,
	backoff: &Backoff,
	stop: &CancellationToken,
) -> Reconnect<T>
where
	E: Display,
	F: FnMut() -> Fut,
	Fut: Future<Output = Result<T, E>>,
{
	let deadline = Instant::now() + backoff.total;
	let mut last = String::from("no attempt was made");
	for attempt in 0.. {
		let wait = backoff.waits.get(attempt).or(backoff.waits.last()).copied().unwrap_or_default();
		let at = Instant::now() + wait;
		if at >= deadline {
			break;
		}
		tokio::select! {
			() = stop.cancelled() => return Reconnect::Stopped,
			() = tokio::time::sleep_until(at) => {},
		}
		let ends = (Instant::now() + backoff.attempt).min(deadline);
		tokio::select! {
			() = stop.cancelled() => return Reconnect::Stopped,
			result = tokio::time::timeout_at(ends, connect()) => match result {
				Ok(Ok(t)) => return Reconnect::Connected(t),
				Ok(Err(e)) => last = e.to_string(),
				Err(_) => last = "the attempt got no answer in time".into(),
			},
		}
		if Instant::now() >= deadline {
			break;
		}
	}
	Reconnect::GaveUp(format!("not reachable for {} s: {last}", backoff.total.as_secs()))
}

/// The drops of one connection, to bound a connection that keeps dropping.
#[derive(Debug, Default)]
pub struct Drops(VecDeque<Instant>);

impl Drops {
	/// One more drop now; `false` when it is one too many within [`DROP_WINDOW`].
	pub fn record(&mut self) -> bool {
		let now = Instant::now();
		while self.0.front().is_some_and(|t| now.duration_since(*t) > DROP_WINDOW) {
			self.0.pop_front();
		}
		self.0.push_back(now);
		self.0.len() <= MAX_DROPS
	}

	/// Why the connection counts as down, for the stop detail.
	pub fn reason() -> String {
		format!(
			"the RPC connection dropped more than {MAX_DROPS} times in {} s",
			DROP_WINDOW.as_secs()
		)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn backoff() -> Backoff {
		Backoff {
			waits: vec![Duration::from_secs(1), Duration::from_secs(2)],
			attempt: Duration::from_secs(3),
			total: Duration::from_secs(15),
		}
	}

	#[tokio::test(start_paused = true)]
	async fn reconnect_backs_off_until_it_connects() {
		let stop = CancellationToken::new();
		let mut tries = 0;
		let started = Instant::now();
		let got = reconnect(
			|| {
				tries += 1;
				std::future::ready(if tries > 3 { Ok(tries) } else { Err("refused") })
			},
			&backoff(),
			&stop,
		)
		.await;
		assert_eq!(got, Reconnect::Connected(4));
		// 1 + 2, then the last wait again: 2 + 2.
		assert_eq!(started.elapsed(), Duration::from_secs(7));
	}

	/// A server that accepts TCP and never finishes the handshake: each attempt times out, and
	/// the node is down at the deadline, not never.
	#[tokio::test(start_paused = true)]
	async fn an_attempt_that_never_answers_ends_at_the_deadline() {
		let stop = CancellationToken::new();
		let started = Instant::now();
		let got: Reconnect<()> =
			reconnect(std::future::pending::<Result<(), String>>, &backoff(), &stop).await;
		let Reconnect::GaveUp(why) = got else { panic!("gave up") };
		assert!(why.contains("not reachable for 15 s"), "{why}");
		assert!(started.elapsed() <= Duration::from_secs(15), "{:?}", started.elapsed());
	}

	#[tokio::test(start_paused = true)]
	async fn cancellation_ends_a_hanging_attempt() {
		let stop = CancellationToken::new();
		let cancel = stop.clone();
		tokio::spawn(async move {
			tokio::time::sleep(Duration::from_millis(1_500)).await;
			cancel.cancel();
		});
		let started = Instant::now();
		let got: Reconnect<()> =
			reconnect(std::future::pending::<Result<(), String>>, &backoff(), &stop).await;
		assert_eq!(got, Reconnect::Stopped);
		assert_eq!(started.elapsed(), Duration::from_millis(1_500));
	}

	#[tokio::test(start_paused = true)]
	async fn a_connection_that_keeps_dropping_is_bounded() {
		let mut drops = Drops::default();
		for _ in 0..MAX_DROPS {
			assert!(drops.record());
			tokio::time::advance(Duration::from_secs(10)).await;
		}
		assert!(!drops.record(), "six drops in 60 s");
		tokio::time::advance(DROP_WINDOW).await;
		assert!(drops.record(), "old drops leave the window");
	}
}
