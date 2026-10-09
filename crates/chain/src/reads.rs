//! Typed reads at one block: events, storage values, runtime API calls. `V` can name only the
//! fields it needs (`DecodeAsType`); a value that doesn't decode is our error (`Fault::Tool`),
//! an RPC that fails is the chain's.

use std::collections::HashSet;

use subxt::dynamic::{self, Value};

use crate::client::{AtBlock, ChainError, decode_err, read_any};

/// One event of a block.
#[derive(Debug, Clone)]
pub struct Event {
	/// The pallet, e.g. `System`.
	pub pallet: String,
	/// The event, e.g. `ExtrinsicFailed`.
	pub name: String,
	/// The extrinsic it belongs to; `None` for events of the block itself.
	pub extrinsic: Option<u32>,
	/// Its fields, read with the `value` helpers.
	pub fields: Value,
}

impl Event {
	/// Whether this is the event `pallet::name`.
	pub fn is(&self, pallet: &str, name: &str) -> bool {
		self.pallet == pallet && self.name == name
	}
}

/// A block's events, in order.
pub async fn events(at: &AtBlock) -> Result<Vec<Event>, ChainError> {
	let events = at.events().fetch().await.map_err(read_any("events"))?;
	let mut out = Vec::new();
	for ev in events.iter() {
		let ev = ev.map_err(decode_err("event"))?;
		let extrinsic = match ev.phase() {
			subxt::events::Phase::ApplyExtrinsic(i) => Some(i),
			_ => None,
		};
		let fields = ev
			.decode_fields_unchecked_as::<Value>()
			.map_err(decode_err("event fields"))?
			.remove_context();
		out.push(Event {
			pallet: ev.pallet_name().to_owned(),
			name: ev.event_name().to_owned(),
			extrinsic,
			fields,
		});
	}
	Ok(out)
}

/// The indices of the extrinsics that failed: those with a `System.ExtrinsicFailed` event.
pub fn failed_extrinsics(events: &[Event]) -> HashSet<u32> {
	events
		.iter()
		.filter(|e| e.is("System", "ExtrinsicFailed"))
		.filter_map(|e| e.extrinsic)
		.collect()
}

/// A plain storage value at a block, `None` when absent; `V` can name only the fields it needs.
pub async fn fetch<V: scale_decode::DecodeAsType>(
	at: &AtBlock,
	pallet: &'static str,
	entry: &'static str,
) -> Result<Option<V>, ChainError> {
	let value = at
		.storage()
		.try_fetch(dynamic::storage::<(), V>(pallet, entry), ())
		.await
		.map_err(read_any(entry))?;
	value.map(|v| v.decode()).transpose().map_err(decode_err(entry))
}

/// A runtime API call at the block, undecoded.
pub async fn runtime_call(
	at: &AtBlock,
	function: &'static str,
	args: &[u8],
) -> Result<Vec<u8>, ChainError> {
	at.runtime_apis()
		.call_raw(function, Some(args))
		.await
		.map_err(read_any(function))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn failed_extrinsics_are_the_system_failures_of_their_extrinsic() {
		let event = |pallet: &str, name: &str, extrinsic| Event {
			pallet: pallet.into(),
			name: name.into(),
			extrinsic,
			fields: Value::unnamed_composite([]),
		};
		let events = [
			event("Balances", "Transfer", Some(0)),
			event("System", "ExtrinsicFailed", Some(1)),
			event("System", "ExtrinsicSuccess", Some(2)),
			event("System", "ExtrinsicFailed", None),
		];
		assert_eq!(failed_extrinsics(&events), HashSet::from([1]));
	}
}
