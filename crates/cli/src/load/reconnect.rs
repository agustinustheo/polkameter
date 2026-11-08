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
