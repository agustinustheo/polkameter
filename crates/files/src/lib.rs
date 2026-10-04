//! The on-disk contract of a run. Every part of a run (load tool, chain recorder, scraper,
//! plugins) writes its own raw file in the run directory; [`read_store`] merges them into the
//! series the checks read. This crate is the one place that knows the formats, so each part can
//! be its own process.
//!
//! | file            | written by        | record              |
//! | --------------- | ----------------- | ------------------- |
//! | `scrapes.jsonl` | scraper           | [`ScrapeRecord`]    |
//! | `blocks.jsonl`  | block follower    | [`BlockRecord`]     |
//! | `load.jsonl`    | load tool         | [`SeriesRecord`]    |
//! | `chain.jsonl`   | chain recorders   | [`SeriesRecord`]    |
//! | `node.jsonl`    | process sampler   | [`NodeSample`]      |

mod metrics;
mod num;
mod problems;
mod records;
pub mod registry;
mod run_dir;
mod series;
pub mod summary;

pub use metrics::{
	PluginMetric, Point, Series, Store, parse_sample_line, parse_samples, read_store,
};
pub use num::{num, to_fixed};
pub use problems::Problems;
pub use records::{
	BlockRecord, BlockStats, FinalStep, Millis, NodeMax, NodeSample, ScrapeRecord, SeriesRecord,
};
pub use run_dir::{JsonlWriter, RunDir};
pub use series::{SeriesOp, SeriesWriter};

/// Now: milliseconds since the Unix epoch. The one clock of a run (ticks, rules, files), so a
/// machine that sleeps shows up as a gap, as it does in the TS tool.
pub fn now_ms() -> Millis {
	std::time::SystemTime::now()
		.duration_since(std::time::UNIX_EPOCH)
		.map_or(0, |d| d.as_millis() as Millis)
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
