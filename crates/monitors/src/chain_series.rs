//! The one writer of a series file such as `chain.jsonl`. Each chain recorder holds a
//! [`ChainSeries`] and sends it typed writes; one task applies them in order and writes the
//! pending counters once a second, as the load tool does for `load.jsonl`.

use std::time::Duration;

use polkameter_files::registry::{Metric, kind};
use polkameter_files::{FileError, JsonlWriter, Millis, SeriesOp, SeriesWriter, now_ms};
use tokio::sync::mpsc;

/// A handle to the `chain.jsonl` writer; cheap to clone.
#[derive(Debug, Clone)]
pub struct ChainSeries(mpsc::UnboundedSender<SeriesOp>);

impl ChainSeries {
	/// Sets a gauge.
	pub fn gauge<const N: usize>(
		&self,
		m: &Metric<kind::Gauge, N>,
		values: [&str; N],
		value: f64,
		t: Millis,
	) {
		let _ = self.0.send(SeriesOp::gauge(m, values, value, t));
	}

	/// Adds `by` to a counter.
	pub fn inc<const N: usize>(
		&self,
		m: &Metric<kind::Counter, N>,
		values: [&str; N],
		by: f64,
		t: Millis,
	) {
		let _ = self.0.send(SeriesOp::inc(m, values, by, t));
	}

	/// Observes one value of a histogram.
	pub fn observe<const N: usize>(
		&self,
		m: &Metric<kind::Histogram, N>,
		values: [&str; N],
		v: f64,
		t: Millis,
	) {
		let _ = self.0.send(SeriesOp::observe(m, values, v, t));
	}
}

/// The writes' queue.
pub type Ops = mpsc::UnboundedReceiver<SeriesOp>;

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
			op = ops.recv() => match op {
				Some(op) => writer.apply(&op)?,
				None => break,
			},
			_ = every.tick() => writer.tick(now_ms())?,
		}
	}
	writer.flush()
}
