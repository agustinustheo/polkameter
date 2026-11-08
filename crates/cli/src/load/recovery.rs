//! After the load: when do new txs land in time again.

use polkameter_plugin_sdk::observers::files::{
	BlockRecord, Millis,
	summary::{Outcome, Probe, ProbePhase},
};

/// The send time of the first of `probes_in_a_row` recovery probes in a row that landed within
/// the threshold, with blocks from then on at about the start interval. `None` while not back.
pub fn recovered_at(
	probes: &[Probe],
	blocks: &[BlockRecord],
	threshold_ms: Millis,
	block_ms: f64,
	rules: &crate::load::rules::Rules,
) -> Option<Millis> {
	let rec: Vec<&Probe> = probes.iter().filter(|p| p.phase == ProbePhase::Recovery).collect();
	let good = |p: &&Probe| {
		p.outcome == Outcome::Included && p.latency_ms.is_some_and(|l| l <= threshold_ms)
	};
	rec.windows(rules.probes_in_a_row).find_map(|w| {
		if !w.iter().all(good) {
			return None;
		}
		let gaps: Vec<u64> = blocks
			.iter()
			.filter(|b| b.seen_at >= w[0].sent_at)
			.filter_map(|b| b.gap_ms)
			.collect();
		if gaps.is_empty() {
			return None;
		}
		let mean = gaps.iter().sum::<u64>() as f64 / gaps.len() as f64;
		(mean <= rules.recovered_block_gap_factor * block_ms).then_some(w[0].sent_at)
	})
}
