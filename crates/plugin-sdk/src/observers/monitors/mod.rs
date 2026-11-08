//! The plugin side of the monitors: [`walker`] follows finalized blocks for a recorder, and
//! [`chain_series`] owns the series file it writes. A monitor never stops the run for a chain
//! error: a node that doesn't answer is a result, recorded in its file and in
//! [`Problems`](crate::observers::Problems). Only our own errors (a file we can't write,
//! an answer that doesn't decode as our types) end a monitor with [`MonitorError`], and then the
//! run.
//!
//! A recorder implements [`walker::Walk`] and runs under [`walker::walk`], which visits every
//! finalized block and reads state once per update. It sends its series to the one task that
//! owns the file ([`chain_series::run`]), through a [`ChainSeries`] from
//! [`chain_series::channel`].

pub mod chain_series;
pub mod walker;

pub use chain_series::ChainSeries;

/// A monitor failed with an error of our own: the run stops.
#[derive(Debug, thiserror::Error)]
pub enum MonitorError {
	/// A raw file could not be written.
	#[error(transparent)]
	File(#[from] crate::observers::files::FileError),
	/// An answer did not decode as our types, or a step edge was missed.
	#[error("{0}")]
	Tool(String),
}
