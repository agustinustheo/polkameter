//! The on-disk contract of a run. Every part of a run (load tool, chain recorder, scraper,
//! plugins) writes its own raw file in the run directory; [`read_store`] merges them into the
//! series the checks read. This module is the one place that knows the formats, so each part can
//! be its own process.
//!
//! | file            | written by        | record              |
//! | --------------- | ----------------- | ------------------- |
//! | `scrapes.jsonl` | scraper           | `ScrapeRecord`      |
//! | `blocks.jsonl`  | block follower    | `BlockRecord`       |
//! | `load.jsonl`    | load tool         | `SeriesRecord`      |
//! | `chain.jsonl`   | chain recorders   | `SeriesRecord`      |
//! | `node.jsonl`    | process sampler   | `NodeSample`        |

mod metrics;
mod num;
mod problems;
mod records;
pub mod registry;
mod run_dir;
mod series;
pub mod summary;

pub use metrics::{PluginMetric, Point, Series, Store, parse_sample_line, read_store};
pub use num::{num, to_fixed};
pub use problems::Problems;
#[doc(hidden)]
pub use records::{BlockRecord, NodeSample, ScrapeRecord};
pub use records::{BlockStats, Millis, NodeMax};
pub use run_dir::{JsonlWriter, RunDir};
pub use series::SeriesWriter;

/// The name a unit variant has in the files, which is its serde name (`"block production"`,
/// `"no result"`): one name per variant, so no table to keep in step with the serde renames.
#[doc(hidden)]
pub fn serde_name(variant: impl serde::Serialize) -> String {
	match serde_json::to_value(variant) {
		Ok(serde_json::Value::String(name)) => name,
		_ => unreachable!("a unit variant serializes as a string"),
	}
}

/// Now: milliseconds since the Unix epoch, on a clock that never goes back. The one clock of a
/// run (ticks, rules, files). The wall clock is read once, the first time a process asks; after
/// that the time advances with the monotonic clock. So a wall-clock step (NTP, a VM resume)
/// neither reverses nor stretches the intervals a run measures, and every timestamp in the run's
/// files is the process's start time plus monotonic elapsed time.
pub fn now_ms() -> Millis {
	static CLOCK: std::sync::OnceLock<EpochClock> = std::sync::OnceLock::new();
	CLOCK.get_or_init(|| EpochClock::new(wall_ms())).now()
}

/// The wall clock now, in milliseconds since the Unix epoch. It can jump either way; use
/// [`now_ms`] for anything that measures time. The load tool compares the two to see a machine
/// that slept.
pub fn wall_ms() -> Millis {
	std::time::SystemTime::now()
		.duration_since(std::time::UNIX_EPOCH)
		.map_or(0, |d| d.as_millis() as Millis)
}

/// Epoch milliseconds from a monotonic clock: the wall time at the start, plus the time elapsed
/// since on the monotonic clock (tokio's, so a test with paused time drives it too).
#[derive(Debug, Clone, Copy)]
pub struct EpochClock {
	start: tokio::time::Instant,
	epoch: Millis,
}

impl EpochClock {
	/// A clock that reads `epoch` now.
	pub fn new(epoch: Millis) -> Self {
		Self { start: tokio::time::Instant::now(), epoch }
	}

	/// Epoch milliseconds now.
	pub fn now(&self) -> Millis {
		self.at(tokio::time::Instant::now())
	}

	/// Epoch milliseconds at `instant` (the start for an instant before it).
	pub fn at(&self, instant: tokio::time::Instant) -> Millis {
		self.epoch + instant.saturating_duration_since(self.start).as_millis() as Millis
	}
}

/// A file of the run could not be read or written: an error of our tools, so the run stops.
#[derive(Debug, thiserror::Error)]
pub enum FileError {
	/// Reading or writing a file failed.
	#[error("{path}: {source}")]
	Io {
		/// The file.
		path: String,
		/// Why.
		source: std::io::Error,
	},
	/// A line of a raw file is not the record it should be.
	#[error("{path}:{line}: {source}")]
	Record {
		/// The file.
		path: String,
		/// 1-based line number.
		line: usize,
		/// Why.
		source: serde_json::Error,
	},
}
