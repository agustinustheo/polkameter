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

pub mod preflight;
pub mod process;
pub mod relay;
pub mod scraper;
pub mod topology;

pub use polkameter_plugin_sdk::observers::monitors::{
	ChainSeries, MonitorError, chain_series, walker,
};
pub use scraper::{Sample, Scraper, ScraperHandle};
pub use topology::{Job, Target, load_targets};
