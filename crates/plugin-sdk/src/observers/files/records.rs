//! Record shapes of the raw files. Field names are the TS tool's (`camelCase`), so both tools
//! read each other's runs.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// Milliseconds since the Unix epoch.
pub type Millis = u64;

/// One scrape of a node's `/metrics` (`scrapes.jsonl`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScrapeRecord {
	/// When the node answered.
	pub t: Millis,
	/// `collator`, `collator-relay` or `validator`.
	pub job: String,
	/// The node's name in zombie.json.
	pub instance: String,
	/// The exposition text, cut to the families in the registry.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub text: Option<String>,
	/// Why the scrape failed: a result, not a tool error.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub error: Option<String>,
}

/// One of our own `polkameter_*` series at a time (`load.jsonl`, `chain.jsonl`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeriesRecord {
	/// When the value last changed.
	pub t: Millis,
	/// A name from [`crate::observers::registry::STRESS_METRICS`].
	pub name: String,
	/// Label values; the registry lists the names.
	pub labels: BTreeMap<String, String>,
	/// The sample.
	#[serde(flatten)]
	pub sample: Sample,
}

/// A gauge or counter value, or a histogram's cumulative buckets.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Sample {
	/// Cumulative counts per bucket in registry order, then `+Inf`.
	Histogram {
		/// Counts.
		buckets: Vec<f64>,
		/// Sum of the observed values.
		sum: f64,
	},
	/// A gauge, or a counter's running total.
	Value {
		/// The value.
		value: f64,
	},
}

/// One block of the chain under load (`blocks.jsonl`): a best block, or one a best block builds on.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockRecord {
	/// Block number.
	pub number: u32,
	/// Block hash, hex.
	pub hash: String,
	/// Wall clock when it arrived as a new best block (a filled-in block: when the best block
	/// on it arrived).
	pub seen_at: Millis,
	/// Never a best block itself: read because a later best block builds on it (after a reorg,
	/// the node announces only the new tip). No gap or interval.
	pub filled_in: bool,
	/// Wall-clock gap to the best block recorded before it.
	pub gap_ms: Option<u64>,
	/// `Timestamp.Now` of the block.
	pub timestamp: Millis,
	/// Extrinsics in the body.
	pub extrinsics: u32,
	/// Of those, ours.
	pub ours: u32,
	/// Of ours, the ones with an `ExtrinsicFailed` event.
	pub ours_failed: u32,
	/// Share of the running runtime's normal limit, in %.
	pub normal_ref_time_pct: f64,
	/// Share of the normal proof size limit, in %.
	pub normal_proof_pct: f64,
	/// The finalized number when the block was recorded.
	pub finalized: u32,
	/// From arrival to read (Rust tool only).
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub fetch_lag_ms: Option<u64>,
}

/// CPU and memory of the node under load (`node.jsonl`), as `ps` reports them, rounded.
/// Fields may be added: build one with [`NodeSample::new`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct NodeSample {
	/// When sampled.
	pub t: Millis,
	/// CPU since the sample before (250 means 2.5 cores busy); null for the first.
	pub cpu_pct: Option<i64>,
	/// Resident memory.
	pub rss_mi_b: u64,
	/// The process sampled: a new one after the node restarted.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub pid: Option<u32>,
}

impl NodeSample {
	/// A sample of process `pid` at `t`.
	pub fn new(t: Millis, cpu_pct: Option<i64>, rss_mi_b: u64, pid: Option<u32>) -> Self {
		Self { t, cpu_pct, rss_mi_b, pid }
	}
}

/// What a run of best blocks held (part of a step record and of recovery).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockStats {
	/// Blocks.
	pub blocks: usize,
	/// Mean wall-clock gap.
	pub mean_block_gap_ms: Option<f64>,
	/// Max wall-clock gap.
	pub max_block_gap_ms: Option<u64>,
	/// Most of our txs in one block.
	pub max_ours_per_block: u32,
	/// Of the normal class limit of the running runtime.
	pub max_normal_ref_time_pct: f64,
	/// Of the normal proof size limit.
	pub max_normal_proof_pct: f64,
}

/// The node's highest CPU and memory in a window, from `node.jsonl`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeMax {
	/// Highest CPU %.
	pub max_cpu_pct: Option<i64>,
	/// Highest resident memory.
	pub max_rss_mi_b: Option<u64>,
}

impl NodeMax {
	/// The highest values among the samples taken between `from` and `to`; `None` without any.
	pub fn over(samples: &[NodeSample], from: Millis, to: Millis) -> Self {
		let inside = samples.iter().filter(|s| s.t >= from && s.t <= to);
		Self {
			max_cpu_pct: inside.clone().filter_map(|s| s.cpu_pct).max(),
			max_rss_mi_b: inside.map(|s| s.rss_mi_b).max(),
		}
	}
}
