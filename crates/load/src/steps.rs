//! What the load tool counts per step and lane, and a step's final numbers ([`FinalStep`],
//! `steps.jsonl`) once all its txs settled.

use std::collections::BTreeMap;

use polkameter_files::{BlockRecord, BlockStats, FinalStep, Millis, NodeMax};

/// Counters of one step of one lane while it runs and settles.
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
	/// Send to best block, per included tx.
	pub latencies_ms: Vec<Millis>,
	/// Send to submit reply.
	pub reply_ms: Vec<Millis>,
	/// Refusals by "code message".
	pub errors: BTreeMap<String, u64>,
	/// Ticks where the source had no tx.
	pub starved_ticks: u64,
	/// Ticks where every connection was backed up.
	pub backpressure_ticks: u64,
	/// Longest wait for a submit reply during the step.
	pub max_oldest_pending_ms: Millis,
	/// Blocks seen while the step ran, with `ours` counted for this lane.
	pub blocks: Vec<BlockRecord>,
}

/// The p-th percentile (nearest rank, as TS); 0 for no values.
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

/// The final numbers of a step; `lane` is `None` in a single-lane run (the TS layout).
pub fn final_step(st: &StepStats, lane: Option<&str>) -> FinalStep {
	let seconds = (st.ended_at.saturating_sub(st.started_at) as f64 / 1000.0).max(0.001);
	let ratio = |a: u64, b: u64, none: f64| if b == 0 { none } else { a as f64 / b as f64 };
	FinalStep {
		step: st.step,
		lane: lane.map(str::to_owned),
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
		included_per_s: st.blocks.iter().map(|b| u64::from(b.ours)).sum::<u64>() as f64 / seconds,
		failed_in_block: st.failed_in_block,
		rejected: st.rejected,
		rejected_ratio: ratio(st.rejected, st.sent, 0.0),
		p50_latency_ms: percentile(&st.latencies_ms, 50.0),
		p95_latency_ms: percentile(&st.latencies_ms, 95.0),
		p95_reply_ms: percentile(&st.reply_ms, 95.0),
		max_oldest_pending_ms: st.max_oldest_pending_ms,
		errors: st.errors.clone(),
		starved_ticks: st.starved_ticks,
		backpressure_ticks: st.backpressure_ticks,
		blocks: block_stats(&st.blocks),
		node: NodeMax::default(),
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
