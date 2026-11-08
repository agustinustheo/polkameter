//! What a plugin needs to observe a run: record series while it runs and judge them after.
//!
//! - The on-disk contract of a run: [`RunDir`], the raw files, the metric [`registry`], recorded
//!   series ([`read_store`]) and `summary.json` ([`summary`]).
//! - Run data for checks: [`RunData`] window math, [`quantile`] and [`Verdict`]s.
//! - Monitors: the finalized-block [`walker`] and the [`chain_series`] writer a chain recorder
//!   runs under.
//!
//! Hidden items are shared with the `polkameter` CLI and are not part of the supported API: they
//! may change in any release.

#![warn(missing_docs, clippy::needless_pass_by_value, clippy::too_many_lines)]

mod checks;
mod files;
mod monitors;

pub use checks::{
	CheckResult, CounterReset, Label, Phase, RunData, Status, Verdict, Window, count_above,
	quantile,
};
#[doc(hidden)]
pub use files::{BlockRecord, NodeSample, ScrapeRecord, serde_name};
pub use files::{
	BlockStats, FileError, JsonlWriter, Millis, NodeMax, PluginMetric, Point, Problems, RunDir,
	Series, SeriesWriter, Store, now_ms, num, parse_sample_line, read_store, registry, summary,
	to_fixed,
};
pub use monitors::{ChainSeries, MonitorError, chain_series, walker};
