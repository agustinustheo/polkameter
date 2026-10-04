//! Baseline, recovery and the loss check: what the run found before and after the load.

use serde::{Deserialize, Serialize};

use crate::records::{BlockStats, Millis, NodeMax};

/// Baseline or recovery.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[allow(missing_docs)]
pub enum ProbePhase {
	Baseline,
	Recovery,
}

/// What became of a probe.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[allow(missing_docs)]
pub enum Outcome {
	Pending,
	Included,
	Failed,
	/// The file word for a probe the node refused at submit.
	Refused,
	Dropped,
}

/// A probe tx, one per block before and after the load.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Probe {
	/// Baseline or recovery.
	pub phase: ProbePhase,
	/// Seconds after its phase started.
	pub sent_at_s: f64,
	/// Send to best block.
	pub latency_ms: Option<Millis>,
	/// What became of it.
	pub outcome: Outcome,
	/// When it was sent (not in the file).
	#[serde(skip)]
	pub sent_at: Millis,
}

/// Probes before the load.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Baseline {
	/// Probes.
	pub probes: usize,
	/// p50 latency.
	pub p50_ms: Millis,
	/// Max latency.
	pub max_ms: Millis,
}

/// After the load stopped: back in time, and the backlog drained.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Recovery {
	/// False when the node closed the RPC connection.
	pub measured: bool,
	/// Probes landed in time again.
	pub recovered: bool,
	/// Seconds from the load stop until then.
	pub seconds: Option<u64>,
	/// What happened.
	pub detail: String,
	/// Landing within this counts as back.
	pub threshold_ms: Millis,
	/// Our flood txs outstanding at the stop.
	pub backlog_at_stop: u64,
	/// ... and at the end.
	pub backlog_at_end: u64,
	/// Seconds until none was left.
	pub drained_seconds: Option<u64>,
	/// Flood txs included per second while recovering.
	pub drain_per_s: f64,
	/// The recovery probes.
	pub probes: Vec<Probe>,
	/// The blocks while recovering.
	pub blocks: BlockStats,
	/// The node while recovering.
	pub node: NodeMax,
}

/// The node's pool counts after the run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodePool {
	/// Unwatched txs in the mempool.
	pub mempool: u64,
	/// Ready txs.
	pub ready: u64,
}

/// Waiting for the last block with our txs to be finalized.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Finality {
	/// The highest block with one of ours.
	pub last_ours_block: u32,
	/// Finalized when the wait ended.
	pub finalized_at: u32,
	/// It was finalized in time.
	pub waited_for_it: bool,
}

/// A scenario's check of chain state for a sample of included txs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StateSample {
	/// Checked.
	pub checked: u64,
	/// Without the state they should have left.
	pub missing: u64,
	/// What was checked.
	pub detail: String,
}

/// Our flood txs counted again on the finalized chain, block by block. The tracker reads best
/// blocks, so a reorg can hide an inclusion from it (the block that replaced the one it read) or
/// keep one the chain dropped (a block on the abandoned fork).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OnChain {
	/// The finalized blocks walked, first and last.
	pub blocks: (u32, u32),
	/// Flood txs in them.
	pub included: u64,
	/// Of those, never seen by the tracker: in a block that replaced one it read.
	pub missed: u64,
	/// Counted by the tracker in a block the finalized chain doesn't have, and in no finalized block.
	pub only_on_fork: u64,
	/// Counted by the tracker in one block, finalized in another.
	pub moved: u64,
}

/// One flood tx the loss check found nowhere: a line of lost.jsonl.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LostTx {
	/// Its hash.
	pub hash: String,
	/// The step that sent it.
	pub step: usize,
	/// When it was sent.
	pub sent_at: Millis,
	/// The tracker counted it in this block, which the finalized chain doesn't have.
	pub on_fork_block: Option<u32>,
	/// The scenario's view of it, e.g. the member and slot of a claim.
	pub scenario: serde_json::Value,
	/// It left the state it should have at the finalized block; `None` when not checked.
	pub state_landed: Option<bool>,
	/// `validate_transaction` at the best block after the run: "valid", or why not.
	pub validate: String,
}

/// Every flood tx is included, refused, expired or ready in the pool; anything else is lost.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Loss {
	/// Successful RPC replies (not a dispatch-success count).
	#[serde(default)]
	pub accepted: u64,
	/// Unresolved submissions without an acceptance reply.
	#[serde(default)]
	pub unknown: u64,
	/// Sent.
	pub sent: u64,
	/// Included.
	pub included: u64,
	/// Failed after inclusion.
	pub failed_in_block: u64,
	/// Refused.
	pub refused: u64,
	/// Expired.
	pub dropped: u64,
	/// Still ready in the node's pool; `None` when it could not be listed.
	pub in_pool: Option<u64>,
	/// Accepted, in no finalized block, refused by nobody, not ready in the pool; `None` when the
	/// pool could not be listed or the finalized chain not walked.
	pub lost: Option<u64>,
	/// The count on the finalized chain; zeros when it could not be walked (see `note`).
	pub on_chain: OnChain,
	/// The node's own counts.
	pub node_pool: Option<NodePool>,
	/// Why something is missing.
	pub note: Option<String>,
	/// The finality wait.
	pub finalized: Option<Finality>,
	/// The state check.
	pub state: Option<StateSample>,
}

impl Outcome {
	/// The name in the files and the summary.
	pub fn name(self) -> &'static str {
		match self {
			Outcome::Pending => "pending",
			Outcome::Included => "included",
			Outcome::Failed => "failed",
			Outcome::Refused => "refused",
			Outcome::Dropped => "dropped",
		}
	}
}
