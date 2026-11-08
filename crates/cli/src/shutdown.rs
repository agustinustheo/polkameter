//! Signal handling for `run`.
//!
//! The first SIGINT or SIGTERM asks the work to stop and starts the grace period. The second
//! signal, or the end of the grace period, forces the stop. The process then exits with
//! 128 + the signal number of the first signal: 130 for SIGINT, 143 for SIGTERM.

use std::{future::Future, time::Duration};

use tokio::sync::mpsc;

/// Default time a stopping run gets to finish its teardown and write its results. With
/// [`FORCE_LIMIT`] it fits the 30 s that Kubernetes and GitLab allow between SIGTERM and SIGKILL.
pub const DEFAULT_GRACE: Duration = Duration::from_secs(20);

/// How long forced work may take to write its outcome before the process gives up on it.
pub const FORCE_LIMIT: Duration = Duration::from_secs(10);

/// A signal that stops the work.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Signal {
	/// SIGINT, or Ctrl-C on Windows.
	Interrupt,
	/// SIGTERM, or Ctrl-Break on Windows.
	Terminate,
}

impl Signal {
	/// The exit code of a process stopped by this signal, after cleanup.
	pub fn exit_code(self) -> i32 {
		match self {
			Self::Interrupt => 130,
			Self::Terminate => 143,
		}
	}

	/// The signal's conventional name.
	pub fn name(self) -> &'static str {
		match self {
			Self::Interrupt => "SIGINT",
			Self::Terminate => "SIGTERM",
		}
	}
}

/// Stop signals, in the order they arrived.
pub struct Signals {
	receiver: mpsc::UnboundedReceiver<Signal>,
}

impl Signals {
	/// Replaces the default handlers of SIGINT and SIGTERM. The handlers are registered before
	/// this returns, so a signal that arrives afterwards is never lost. Must be called inside a
	/// Tokio runtime.
	pub fn install() -> std::io::Result<Self> {
		let (sender, receiver) = mpsc::unbounded_channel();
		#[cfg(unix)]
		{
			use tokio::signal::unix::{SignalKind, signal};
			for (kind, value) in [
				(SignalKind::interrupt(), Signal::Interrupt),
				(SignalKind::terminate(), Signal::Terminate),
			] {
				let mut stream = signal(kind)?;
				let sender = sender.clone();
				tokio::spawn(async move {
					while stream.recv().await.is_some() {
						if sender.send(value).is_err() {
							break;
						}
					}
				});
			}
		}
		#[cfg(windows)]
		{
			let mut interrupt = tokio::signal::windows::ctrl_c()?;
			let mut terminate = tokio::signal::windows::ctrl_break()?;
			let (first, second) = (sender.clone(), sender);
			tokio::spawn(async move {
				while interrupt.recv().await.is_some() {
					if first.send(Signal::Interrupt).is_err() {
						break;
					}
				}
			});
			tokio::spawn(async move {
				while terminate.recv().await.is_some() {
					if second.send(Signal::Terminate).is_err() {
						break;
					}
				}
			});
		}
		Ok(Self { receiver })
	}

	/// The next signal. Waits forever if no more can arrive.
	pub async fn recv(&mut self) -> Signal {
		match self.receiver.recv().await {
			Some(signal) => signal,
			None => std::future::pending().await,
		}
	}
}

/// What `supervise` saw: the work's result unless it was abandoned, and the first signal.
pub struct Supervised<T> {
	/// The work's result, or `None` if it did not finish within [`FORCE_LIMIT`] after a forced
	/// stop and was dropped.
	pub result: Option<T>,
	/// The first stop signal, if any arrived.
	pub signal: Option<Signal>,
}

/// Drives `work` to completion while handling stop signals.
///
/// On the first signal, `request` is called once. On a second signal, or when `grace` has passed
/// since the first, `force` is called once. After a forced stop the work gets [`FORCE_LIMIT`] to
/// finish, then it is dropped.
pub async fn supervise<T>(
	work: impl Future<Output = T>,
	signals: &mut Signals,
	grace: Duration,
	mut request: impl FnMut(Signal),
	mut force: impl FnMut(Signal),
) -> Supervised<T> {
	tokio::pin!(work);
	let mut first = None;
	let mut forced = false;
	let grace_end = tokio::time::sleep(grace);
	tokio::pin!(grace_end);
	let force_end = tokio::time::sleep(FORCE_LIMIT);
	tokio::pin!(force_end);
	loop {
		tokio::select! {
			biased;
			result = &mut work => return Supervised { result: Some(result), signal: first },
			signal = signals.recv(), if !forced => match first {
				None => {
					first = Some(signal);
					grace_end.as_mut().reset(tokio::time::Instant::now() + grace);
					request(signal);
				},
				Some(first) => {
					forced = true;
					force_end.as_mut().reset(tokio::time::Instant::now() + FORCE_LIMIT);
					force(first);
				},
			},
			() = &mut grace_end, if first.is_some() && !forced => {
				forced = true;
				force_end.as_mut().reset(tokio::time::Instant::now() + FORCE_LIMIT);
				force(first.unwrap_or(Signal::Interrupt));
			},
			() = &mut force_end, if forced => return Supervised { result: None, signal: first },
		}
	}
}
