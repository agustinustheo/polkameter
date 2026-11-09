//! What the load tool counts per step, and a step's final numbers ([`FinalStep`],
//! `steps.jsonl`) once all its txs settled.

use std::collections::BTreeMap;

use polkameter_plugin_sdk::observers::{BlockRecord, BlockStats, Millis, NodeMax};
use serde::{Deserialize, Serialize};

/// A step's final numbers once all its txs settled (`steps.jsonl`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FinalStep {
	/// Step index.
	pub step: u32,
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
	/// Included txs whose latency is left out: seen late, after PolkaMeter was paused or while
	/// its block follower caught up after a reconnect.
	#[serde(default)]
	pub untimed: u64,
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

/// Counters of one step while it runs and settles.
#[derive(Debug, Clone, Default)]
pub struct StepStats {
	/// Step index.
	pub step: u32,
	/// Target send rate.
	pub target_rate: f64,
	/// Start.
	pub started_at: Millis,
	/// End.
	pub ended_at: Millis,
	/// Sent.
	pub sent: u64,
	/// Later seen in a best block.
	pub included: u64,
	/// Of those, with an `ExtrinsicFailed` event.
	pub failed_in_block: u64,
	/// Refused by the node.
	pub rejected: u64,
	/// Send to best block, per included tx timed while PolkaMeter observed.
	pub latencies_ms: Vec<Millis>,
	/// Included txs left out of `latencies_ms`: seen late, after PolkaMeter was paused or while
	/// its block follower caught up after a reconnect.
	pub untimed: u64,
	/// Send to submit reply.
	pub reply_ms: Vec<Millis>,
	/// Refusals by "code message".
	pub errors: BTreeMap<String, u64>,
	/// Ticks where the source had no tx.
	pub starved_ticks: u64,
	/// Ticks where every open connection was backed up.
	pub backpressure_ticks: u64,
	/// Ticks where no connection was open: all reconnecting.
	pub disconnected_ticks: u64,
	/// Longest wait for a submit reply during the step.
	pub max_oldest_pending_ms: Millis,
	/// Blocks seen while the step ran.
	pub blocks: Vec<BlockRecord>,
	/// The step ended.
	pub closed: bool,
	/// New best blocks that arrived while the step ran.
	pub heads: u64,
	/// The longest time without a new best block inside the step's own time: a gap that spans
	/// a step edge counts in each step for the part inside it.
	pub max_gap_ms: Option<Millis>,
	/// Time the block follower was disconnected while the step ran.
	pub unobserved_ms: Millis,
}

impl StepStats {
	/// No new best block from `from` to `to`: keeps the part inside the step's time.
	pub fn gap_portion(&mut self, from: Millis, to: Millis) {
		let end = if self.closed { to.min(self.ended_at) } else { to };
		let part = end.saturating_sub(from.max(self.started_at));
		if end > self.started_at && part > 0 {
			self.max_gap_ms = Some(self.max_gap_ms.map_or(part, |m| m.max(part)));
		}
	}
}

/// The p-th percentile (nearest rank); 0 for no values.
pub fn percentile(values: &[Millis], p: f64) -> Millis {
	if values.is_empty() {
		return 0;
	}
	let mut sorted = values.to_vec();
	sorted.sort_unstable();
	let i = ((p / 100.0) * sorted.len() as f64).floor() as usize;
	sorted[i.min(sorted.len() - 1)]
}

/// What a run of best blocks held.
pub fn block_stats(blocks: &[BlockRecord]) -> BlockStats {
	let gaps: Vec<u64> = blocks.iter().filter_map(|b| b.gap_ms).collect();
	BlockStats {
		blocks: blocks.len(),
		mean_block_gap_ms: (!gaps.is_empty())
			.then(|| gaps.iter().sum::<u64>() as f64 / gaps.len() as f64),
		max_block_gap_ms: gaps.iter().copied().max(),
		max_ours_per_block: blocks.iter().map(|b| b.ours).max().unwrap_or(0),
		max_normal_ref_time_pct: blocks.iter().map(|b| b.normal_ref_time_pct).fold(0.0, f64::max),
		max_normal_proof_pct: blocks.iter().map(|b| b.normal_proof_pct).fold(0.0, f64::max),
	}
}

/// The final numbers of a step.
pub fn final_step(st: &StepStats) -> FinalStep {
	let seconds = (st.ended_at.saturating_sub(st.started_at) as f64 / 1000.0).max(0.001);
	let ratio = |a: u64, b: u64, none: f64| if b == 0 { none } else { a as f64 / b as f64 };
	FinalStep {
		step: st.step,
		target_rate: st.target_rate,
		seconds: (seconds * 10.0).round() / 10.0,
		sent: st.sent,
		sent_per_s: st.sent as f64 / seconds,
		send_ratio: if st.target_rate == 0.0 {
			1.0
		} else {
			st.sent as f64 / (st.target_rate * seconds)
		},
		included: st.included,
		included_ratio: ratio(st.included, st.sent, 1.0),
		// The step's own txs, wherever they were included: not the step before's backlog.
		included_per_s: st.included as f64 / seconds,
		failed_in_block: st.failed_in_block,
		rejected: st.rejected,
		rejected_ratio: ratio(st.rejected, st.sent, 0.0),
		untimed: st.untimed,
		p50_latency_ms: percentile(&st.latencies_ms, 50.0),
		p95_latency_ms: percentile(&st.latencies_ms, 95.0),
		p95_reply_ms: percentile(&st.reply_ms, 95.0),
		max_oldest_pending_ms: st.max_oldest_pending_ms,
		errors: st.errors.clone(),
		starved_ticks: st.starved_ticks,
		backpressure_ticks: st.backpressure_ticks,
		blocks: step_blocks(st),
		node: NodeMax::default(),
	}
}

/// A step's blocks; the gaps by the step's own time: the mean is the time observed over the new
/// best blocks that arrived in it (all of it when none did, so a step without a block is as slow
/// as it is long), the max the longest stretch inside it without one.
fn step_blocks(st: &StepStats) -> BlockStats {
	let observed = st.ended_at.saturating_sub(st.started_at).saturating_sub(st.unobserved_ms);
	BlockStats {
		mean_block_gap_ms: (observed > 0).then(|| observed as f64 / st.heads.max(1) as f64),
		max_block_gap_ms: st.max_gap_ms,
		..block_stats(&st.blocks)
	}
}

#[cfg(test)]
mod tests {
	use super::percentile;

	#[test]
	fn percentile_is_nearest_rank() {
		assert_eq!(percentile(&[1, 5, 9, 100], 95.0), 100);
		assert_eq!(percentile(&[1, 5, 9, 100], 50.0), 9);
		assert_eq!(percentile(&[9, 1, 100, 5], 0.0), 1);
		assert_eq!(percentile(&[], 95.0), 0);
	}
}
