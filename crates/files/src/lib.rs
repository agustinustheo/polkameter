//! The on-disk contract of a stress run. Every part of a run (load tool, chain recorder,
//! scraper) writes its own raw file in `results/<run id>/`; the build step merges them into
//! `run.om`, and the evaluator reads only that. This crate is the one place that knows the
//! formats, so each part can be its own process.
//!
//! | file            | written by        | record              |
//! | --------------- | ----------------- | ------------------- |
//! | `scrapes.jsonl` | scraper           | [`ScrapeRecord`]    |
//! | `blocks.jsonl`  | block follower    | [`BlockRecord`]     |
//! | `load.jsonl`    | load tool         | [`SeriesRecord`]    |
//! | `chain.jsonl`   | chain recorders   | [`SeriesRecord`]    |
//! | `node.jsonl`    | process sampler   | [`NodeSample`]      |
//! | `run.om`        | build             | OpenMetrics text    |

mod num;
mod openmetrics;
mod problems;
mod records;
pub mod registry;
mod run_dir;
mod series;
pub mod summary;

pub use num::{num, to_fixed};
pub use openmetrics::{
	Point, Series, Store, build_run_om, canonical_number, parse_run_om, parse_sample_line,
};
pub use problems::Problems;
pub use records::{
	BlockRecord, BlockStats, FinalStep, Millis, NodeMax, NodeSample, ScrapeRecord, SeriesRecord,
};
pub use run_dir::{JsonlWriter, RunDir};
pub use series::{SeriesOp, SeriesWriter};

/// People's para id on previewnet: the `para` label of the relay recorder's series.
pub const PEOPLE_PARA_ID: &str = "1502";

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
