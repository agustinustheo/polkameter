//! The monitors of a run. Each one is a task that writes its own raw file and never stops the
//! run for a chain error: a node that doesn't answer is a result, recorded in its file and in
//! [`Problems`]. Only our own errors (a file we can't write, an answer that doesn't decode as
//! our types) end a monitor with [`MonitorError`], and then the run.
//!
//! | monitor           | reads                                 | writes          |
//! | ----------------- | ------------------------------------- | --------------- |
//! | scraper           | every node's `/metrics`               | `scrapes.jsonl` |
//! | relay recorder    | People inclusion, disputes, slots     | `chain.jsonl`   |
//! | recycler recorder | Recycler backlog and maintenance      | `chain.jsonl`   |
//! | process sampler   | the People node's CPU and memory      | `node.jsonl`    |
//!
//! The two chain recorders walk finalized blocks ([`walker`]) and send their series to the one
//! task that owns `chain.jsonl` ([`chain_series`]). The block follower is not here: the load
//! tool drives it, because it matches our txs in each block.

pub mod chain_series;
pub mod preflight;
pub mod process;
pub mod recycler;
pub mod relay;
pub mod scraper;
pub mod topology;
pub mod walker;

pub use chain_series::ChainSeries;
pub use polkameter_files::Problems;
pub use scraper::{Sample, Scraper, ScraperHandle};
pub use topology::{Job, Target, load_targets};

/// A monitor failed with an error of our own: the run stops.
#[derive(Debug, thiserror::Error)]
pub enum MonitorError {
	/// A raw file could not be written.
	#[error(transparent)]
	File(#[from] polkameter_files::FileError),
	/// An answer did not decode as our types, or a step edge was missed.
	#[error("{0}")]
	Tool(String),
}
