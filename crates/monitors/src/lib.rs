//! The monitors of a run. Each one is a task that writes its own raw file and never stops the
//! run for a chain error: a node that doesn't answer is a result, recorded in its file and in
//! `polkameter_files::Problems`. Only our own errors (a file we can't write, an answer that
//! doesn't decode as our types) end a monitor with [`MonitorError`], and then the run.
//!
//! | monitor         | reads                                          | writes          |
//! | --------------- | ---------------------------------------------- | --------------- |
//! | scraper         | every node's `/metrics`                        | `scrapes.jsonl` |
//! | relay recorder  | inclusion, disputes and slots of each parachain | `chain.jsonl`   |
//! | process sampler | the CPU and memory of the node under load      | `node.jsonl`    |
//!
//! Chain recorders implement [`walker::Walk`] and run under [`walker::walk`], which visits every
//! finalized block and reads state once per update. They send their series to the one task that
//! owns `chain.jsonl` ([`chain_series`]). A plugin runs its own recorder the same way, writing to
//! a series from [`chain_series::channel`] in its plugin directory. The block follower is not
//! here: the load tool drives it, because it matches our txs in each block.

pub mod chain_series;
pub mod preflight;
pub mod process;
pub mod relay;
pub mod scraper;
pub mod topology;
pub mod walker;

/// Part of the plugin API (used by out-of-tree plugins).
pub use chain_series::ChainSeries;
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
