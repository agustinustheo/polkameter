//! Records what the relay did for each parachain, at finalized relay blocks only (the relay
//! forks even at idle, so best blocks would count some candidates twice). Per block:
//!
//! - `ParaInclusion.CandidateBacked`, `CandidateIncluded` and `CandidateTimedOut`, by para;
//! - `ParasDisputes.DisputeInitiated` (the event names only the candidate, not the para);
//! - the slots offered to each para: the cores whose claim queue starts with it.
//!
//! Missed slots for People = slots offered − candidates included (outcomes.md, PVF level 1).

use std::collections::BTreeMap;

use parity_scale_codec::Decode;
use polkameter_chain::value::{as_u64, field, nth};
use polkameter_chain::{ChainError, Client, events, runtime_call};
use polkameter_files::now_ms;
use polkameter_files::registry::{
	PARA_BACKED, PARA_INCLUDED, PARA_SLOTS, PARA_TIMED_OUT, RELAY_DISPUTES, RELAY_FINALIZED_BLOCKS,
};

use crate::chain_series::ChainSeries;
use crate::walker::Walk;

/// The relay recorder.
pub struct RelayRecorder {
	client: Client,
	series: ChainSeries,
}

impl RelayRecorder {
	/// Records `client`'s finalized blocks to `series`.
	pub fn new(client: Client, series: ChainSeries) -> Self {
		Self { client, series }
	}
}

impl Walk for RelayRecorder {
	const NAME: &'static str = "relay recorder";

	async fn on_block(&mut self, _number: u32, hash: [u8; 32]) -> Result<(), ChainError> {
		let at = self.client.at(hash).await?;
		let (events, queue) =
			tokio::join!(events(&at), runtime_call(&at, "ParachainHost_claim_queue", &[]));
		let (events, queue) = (events?, queue?);
		// BTreeMap<CoreIndex, VecDeque<ParaId>>: the same bytes as a list of (u32, Vec<u32>).
		let queue: Vec<(u32, Vec<u32>)> = Decode::decode(&mut &queue[..]).map_err(|e| {
			ChainError::Decode { what: "ParachainHost_claim_queue", detail: e.to_string() }
		})?;
		let now = now_ms();
		self.series.inc(&RELAY_FINALIZED_BLOCKS, [], 1.0, now);
		let mut slots: BTreeMap<u32, f64> = BTreeMap::new();
		for para in queue.iter().filter_map(|(_, q)| q.first()) {
			*slots.entry(*para).or_default() += 1.0;
		}
		for (para, n) in slots {
			self.series.inc(&PARA_SLOTS, [&para.to_string()], n, now);
		}
		for (pallet, name, _, fields) in &events {
			if pallet == "ParasDisputes" && name == "DisputeInitiated" {
				self.series.inc(&RELAY_DISPUTES, [], 1.0, now);
				continue;
			}
			if pallet != "ParaInclusion" {
				continue;
			}
			let metric = match name.as_str() {
				"CandidateBacked" => &PARA_BACKED,
				"CandidateIncluded" => &PARA_INCLUDED,
				"CandidateTimedOut" => &PARA_TIMED_OUT,
				_ => continue,
			};
			// The first field is the candidate receipt: its descriptor names the para.
			if let Some(para) =
				nth(fields, 0).and_then(|receipt| field(receipt, "para_id")).and_then(as_u64)
			{
				self.series.inc(metric, [&para.to_string()], 1.0, now);
			}
		}
		Ok(())
	}
}
