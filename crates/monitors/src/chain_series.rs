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
pub type Write = Box<dyn FnOnce(&mut SeriesWriter) + Send>;

/// A handle to the `chain.jsonl` writer; cheap to clone.
#[derive(Debug, Clone)]
pub struct ChainSeries(mpsc::UnboundedSender<Write>);

impl ChainSeries {
	/// Adds `by` to a counter.
	pub fn inc<const N: usize>(
		&self,
		m: &'static Metric<kind::Counter, N>,
		values: [&str; N],
		by: f64,
		t: Millis,
	) {
		let values = values.map(str::to_owned);
		let _ = self.0.send(Box::new(move |w| {
			w.inc(m, values.each_ref().map(String::as_str), by, t);
		}));
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
				Some(write) => write(&mut writer),
				None => break,
			},
			_ = every.tick() => writer.tick(now_ms())?,
		}
	}
	writer.flush()
}
