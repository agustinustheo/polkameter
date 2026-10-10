//! Records what the relay did for each parachain, at finalized relay blocks only (the relay
//! forks even at idle, so best blocks would count some candidates twice). Per block:
//!
//! - `ParaInclusion.CandidateBacked`, `CandidateIncluded` and `CandidateTimedOut`, by para;
//! - `ParasDisputes.DisputeInitiated` (the event names only the candidate, not the para);
//! - the slots offered to each para: the cores whose claim queue starts with it.
//!
//! Missed slots for a parachain = slots offered − candidates included (outcomes.md, PVF level 1).

use std::collections::BTreeMap;

use parity_scale_codec::Decode;
use polkameter_chain::{
	ChainError, Client, decode_err, events, runtime_call,
	value::{as_u64, field, nth},
};
use polkameter_files::{
	Problems, now_ms,
	registry::{
		PARA_BACKED, PARA_INCLUDED, PARA_SLOTS, PARA_TIMED_OUT, RELAY_DISPUTES,
		RELAY_FINALIZED_BLOCKS,
	},
};
use tokio_util::sync::CancellationToken;

use crate::{
	MonitorError,
	chain_series::ChainSeries,
	walker::{self, Walk},
};

/// Records every finalized block of the relay `client` to `series` until `stop`.
pub async fn walk(
	client: Client,
	series: ChainSeries,
	stop: CancellationToken,
	problems: Problems,
) -> Result<(), MonitorError> {
	walker::walk(client.clone(), RelayRecorder { client, series }, stop, problems).await
}

/// The relay recorder: one block of the relay at a time.
struct RelayRecorder {
	client: Client,
	series: ChainSeries,
}

impl Walk for RelayRecorder {
	const NAME: &'static str = "relay recorder";

	async fn on_block(&mut self, _number: u32, hash: [u8; 32]) -> Result<(), ChainError> {
		self.record(hash).await
	}
}

impl RelayRecorder {
	/// Counts what the relay did in one finalized block.
	async fn record(&self, hash: [u8; 32]) -> Result<(), ChainError> {
		let at = self.client.at(hash).await?;
		let (events, queue) =
			tokio::join!(events(&at), runtime_call(&at, "ParachainHost_claim_queue", &[]));
		let (events, queue) = (events?, queue?);
		// BTreeMap<CoreIndex, VecDeque<ParaId>>: the same bytes as a list of (u32, Vec<u32>).
		let queue: Vec<(u32, Vec<u32>)> =
			Decode::decode(&mut &queue[..]).map_err(decode_err("ParachainHost_claim_queue"))?;
		let now = now_ms();
		self.series.inc(&RELAY_FINALIZED_BLOCKS, [], 1.0, now);
		let mut slots: BTreeMap<u32, f64> = BTreeMap::new();
		for para in queue.iter().filter_map(|(_, q)| q.first()) {
			*slots.entry(*para).or_default() += 1.0;
		}
		for (para, n) in slots {
			self.series.inc(&PARA_SLOTS, [&para.to_string()], n, now);
		}
		for ev in &events {
			if ev.is("ParasDisputes", "DisputeInitiated") {
				self.series.inc(&RELAY_DISPUTES, [], 1.0, now);
				continue;
			}
			if ev.pallet != "ParaInclusion" {
				continue;
			}
			let metric = match ev.name.as_str() {
				"CandidateBacked" => &PARA_BACKED,
				"CandidateIncluded" => &PARA_INCLUDED,
				"CandidateTimedOut" => &PARA_TIMED_OUT,
				_ => continue,
			};
			// The first field is the candidate receipt: its descriptor names the para.
			if let Some(para) = nth(&ev.fields, 0)
				.and_then(|receipt| field(receipt, "para_id"))
				.and_then(as_u64)
			{
				self.series.inc(metric, [&para.to_string()], 1.0, now);
			}
		}
		Ok(())
	}
}
