//! What a plugin needs to observe a run: record series while it runs and judge them after.
//!
//! - [`files`]: the on-disk contract of a run (run directory, raw files, metric registry,
//!   recorded series, `summary.json`).
//! - [`checks`]: the evaluator (run data, windows, quantiles, verdicts).
//! - [`monitors`]: the finalized-block [`walker`] and the [`chain_series`] writer a chain
//!   recorder runs under.
//!
//! The items a plugin uses are also re-exported here, so
//! `polkameter_plugin_sdk::observers::{RunData, RunDir, walker, …}` is enough. Hidden items in
//! [`checks`] and [`files`] are shared with the `polkameter` CLI and are not part of the
//! supported API: they may change in any release.

#![warn(missing_docs, clippy::needless_pass_by_value, clippy::too_many_lines)]

pub mod checks;
pub mod files;
pub mod monitors;

pub use checks::{
	CheckResult, CounterReset, Label, Phase, RunData, Status, Verdict, Window, count_above,
	quantile,
};
pub use files::{
	FileError, JsonlWriter, Millis, PluginMetric, Point, Problems, RunDir, Series, SeriesWriter,
	Store, now_ms, num, parse_sample_line, read_store, registry, summary, to_fixed,
};
pub use monitors::{ChainSeries, MonitorError, chain_series, walker};
