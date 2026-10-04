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
	/// A name from [`crate::registry::STRESS_METRICS`].
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
	/// Timestamp gap to the block recorded before it. On a fork it comes in 0 s / 12 s pairs; use `gap_ms`.
	pub interval_ms: Option<i64>,
	/// Extrinsics in the body.
	pub extrinsics: u32,
	/// Of those, ours.
	pub ours: u32,
	/// Of ours, the ones with an `ExtrinsicFailed` event.
	pub ours_failed: u32,
	/// Body size.
	pub bytes: u64,
	/// `System.BlockWeight` of the normal class.
	pub normal_ref_time: u64,
	/// Proof size of the normal class.
	pub normal_proof_size: u64,
	/// Share of the running runtime's normal limit, in %.
	pub normal_ref_time_pct: f64,
	/// Share of the normal proof size limit, in %.
	pub normal_proof_pct: f64,
	/// Operational class ref time.
	pub operational_ref_time: u64,
	/// Mandatory class ref time.
	pub mandatory_ref_time: u64,
	/// The finalized number when the block was recorded.
	pub finalized: u32,
	/// From arrival to read (Rust tool only).
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub fetch_lag_ms: Option<u64>,
}

/// CPU and memory of the node under load (`node.jsonl`), as `ps` reports them, rounded.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NodeSample {
	/// When sampled.
	pub t: Millis,
	/// CPU since the sample before (250 means 2.5 cores busy); null for the first.
	pub cpu_pct: Option<i64>,
	/// Resident memory.
	pub rss_mi_b: u64,
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
	/// Largest body.
	pub max_block_bytes: u64,
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

/// A step's final numbers once all its txs settled (`steps.jsonl`), per lane.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FinalStep {
	/// Step index.
	pub step: u32,
	/// The lane (its tx call); absent in single-lane runs of the TS tool.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub lane: Option<String>,
	/// Target rate.
	pub target_rate: f64,
	/// Length, 0.1 s.
	pub seconds: f64,
	/// Sent.
	pub sent: u64,
	/// Sent per second.
	pub sent_per_s: f64,
	/// Sent against target.
	pub send_ratio: f64,
	/// Included.
	pub included: u64,
	/// Included against sent (1 with nothing sent).
	pub included_ratio: f64,
	/// Our txs in blocks seen during the step, per second: what the chain took.
	pub included_per_s: f64,
	/// Failed after inclusion.
	pub failed_in_block: u64,
	/// Refused at submit.
	pub rejected: u64,
	/// Refused against sent.
	pub rejected_ratio: f64,
	/// Expired unseen.
	pub dropped: u64,
	/// p50 send to best block.
	pub p50_latency_ms: Millis,
	/// p95 send to best block.
	pub p95_latency_ms: Millis,
	/// p95 submit reply.
	pub p95_reply_ms: Millis,
	/// Longest wait for a reply.
	pub max_oldest_pending_ms: Millis,
	/// Refusals by "code message".
	pub errors: BTreeMap<String, u64>,
	/// Ticks without a tx.
	pub starved_ticks: u64,
	/// Ticks with every connection backed up.
	pub backpressure_ticks: u64,
	/// The step's blocks.
	#[serde(flatten)]
	pub blocks: BlockStats,
	/// The node's highest CPU and memory.
	pub node: NodeMax,
}
