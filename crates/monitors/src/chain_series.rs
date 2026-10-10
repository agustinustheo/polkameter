//! The one writer of a series file such as `chain.jsonl`. Each chain recorder holds a
//! [`ChainSeries`] and sends it typed writes; one task applies them in order and writes the
//! pending counters once a second, as the load tool does for `load.jsonl`.

use std::time::Duration;

use polkameter_files::{
	FileError, JsonlWriter, Millis, SeriesWriter, now_ms,
	registry::{Metric, kind},
};
use tokio::sync::mpsc;

/// One write, made by a handle and applied by the file's owner.
pub type Write = Box<dyn FnOnce(&mut SeriesWriter) -> Result<(), FileError> + Send>;

/// A handle to the `chain.jsonl` writer; cheap to clone.
#[derive(Debug, Clone)]
pub struct ChainSeries(mpsc::UnboundedSender<Write>);

impl ChainSeries {
	/// Sets a gauge.
	/// Part of the plugin API (used by out-of-tree plugins).
	pub fn gauge<const N: usize>(
		&self,
		m: &'static Metric<kind::Gauge, N>,
		values: [&str; N],
		value: f64,
		t: Millis,
	) {
		let values = values.map(str::to_owned);
		self.send(move |w| w.gauge(m, values.each_ref().map(String::as_str), value, t));
	}

	/// Adds `by` to a counter.
	pub fn inc<const N: usize>(
		&self,
		m: &'static Metric<kind::Counter, N>,
		values: [&str; N],
		by: f64,
		t: Millis,
	) {
		let values = values.map(str::to_owned);
		self.send(move |w| {
			w.inc(m, values.each_ref().map(String::as_str), by, t);
			Ok(())
		});
	}

	/// Observes one value of a histogram.
	/// Part of the plugin API (used by out-of-tree plugins).
	pub fn observe<const N: usize>(
		&self,
		m: &'static Metric<kind::Histogram, N>,
		values: [&str; N],
		v: f64,
		t: Millis,
	) {
		let values = values.map(str::to_owned);
		self.send(move |w| {
			w.observe(m, values.each_ref().map(String::as_str), v, t);
			Ok(())
		});
	}

	/// Queues a write. A send fails only once the writer has stopped, at the end of a run; the
	/// write is then dropped.
	fn send(
		&self,
		write: impl FnOnce(&mut SeriesWriter) -> Result<(), FileError> + Send + 'static,
	) {
		let _ = self.0.send(Box::new(write));
	}
}

/// The writes' queue.
pub type Ops = mpsc::UnboundedReceiver<Write>;

/// A handle and the queue its writes land in.
pub fn channel() -> (ChainSeries, Ops) {
	let (tx, rx) = mpsc::unbounded_channel();
	(ChainSeries(tx), rx)
}

/// Applies every write to `out` until the last handle is dropped, then writes the file out.
pub async fn run(out: JsonlWriter, mut ops: Ops) -> Result<(), FileError> {
	let mut writer = SeriesWriter::new(out);
	let mut every = tokio::time::interval(Duration::from_secs(1));
	loop {
		tokio::select! {
			write = ops.recv() => match write {
				Some(write) => write(&mut writer)?,
				None => break,
			},
			_ = every.tick() => writer.tick(now_ms())?,
		}
	}
	writer.flush()
}
