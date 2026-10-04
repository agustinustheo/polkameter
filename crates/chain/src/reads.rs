//! Typed reads at one block: events, storage values and maps, a block's calls, runtime API
//! calls. `V` can name only the fields it needs (`DecodeAsType`); a value that doesn't decode
//! is our error (`Fault::Tool`), an RPC that fails is the chain's.

use subxt::dynamic::{self, Value};

use crate::client::{AtBlock, ChainError, decode_err, read_any};

/// A block's events, flat: (pallet, event, extrinsic index, fields).
pub async fn events(at: &AtBlock) -> Result<Vec<(String, String, Option<u32>, Value)>, ChainError> {
	let events = at.events().fetch().await.map_err(read_any("events"))?;
	let mut out = Vec::new();
	for ev in events.iter() {
		let ev = ev.map_err(decode_err("event"))?;
		let index = match ev.phase() {
			subxt::events::Phase::ApplyExtrinsic(i) => Some(i),
			_ => None,
		};
		let fields = ev
			.decode_fields_unchecked_as::<Value>()
			.map_err(decode_err("event fields"))?
			.remove_context();
		out.push((ev.pallet_name().to_owned(), ev.event_name().to_owned(), index, fields));
	}
	Ok(out)
}

/// A storage value at a block, `None` when absent; `V` can name only the fields it needs.
pub async fn fetch<K, V>(
	at: &AtBlock,
	pallet: &'static str,
	entry: &'static str,
	keys: K,
) -> Result<Option<V>, ChainError>
where
	K: frame_decode::storage::IntoEncodableValues + frame_decode::storage::IntoDecodableValues,
	V: scale_decode::DecodeAsType,
{
	let value = at
		.storage()
		.try_fetch(dynamic::storage::<K, V>(pallet, entry), keys)
		.await
		.map_err(read_any(entry))?;
	value.map(|v| v.decode()).transpose().map_err(decode_err(entry))
}

/// True when the map `pallet.entry` has any key that starts with `prefix` at the block.
pub async fn has_prefix<K, P>(
	at: &AtBlock,
	pallet: &'static str,
	entry: &'static str,
	prefix: P,
) -> Result<bool, ChainError>
where
	K: frame_decode::storage::IntoEncodableValues + frame_decode::storage::IntoDecodableValues,
	P: subxt::storage::PrefixOf<K>,
{
	let mut entries = at
		.storage()
		.iter(dynamic::storage::<K, Value>(pallet, entry), prefix)
		.await
		.map_err(read_any(entry))?;
	Ok(entries.next().await.transpose().map_err(read_any(entry))?.is_some())
}

/// The pallet and call name of every extrinsic of a block, in order; `None` for one subxt
/// cannot decode (an unknown extension version, another chain's format).
pub async fn calls(at: &AtBlock) -> Result<Vec<Option<(String, String)>>, ChainError> {
	let exts = at.extrinsics().fetch().await.map_err(read_any("extrinsics"))?;
	Ok(exts
		.iter()
		.map(|e| e.ok().map(|e| (e.pallet_name().to_owned(), e.call_name().to_owned())))
		.collect())
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

/// Every entry of the map `pallet.entry` at the block, keys and values decoded as our types.
pub async fn entries<K, V>(
	at: &AtBlock,
	pallet: &'static str,
	entry: &'static str,
) -> Result<Vec<(K, V)>, ChainError>
where
	K: frame_decode::storage::IntoEncodableValues + frame_decode::storage::IntoDecodableValues,
	V: scale_decode::DecodeAsType,
	(): subxt::storage::PrefixOf<K>,
{
	let mut it = at
		.storage()
		.iter(dynamic::storage::<K, V>(pallet, entry), ())
		.await
		.map_err(read_any(entry))?;
	let mut out = Vec::new();
	while let Some(kv) = it.next().await.transpose().map_err(read_any(entry))? {
		let key = kv.key().map_err(decode_err(entry))?.decode().map_err(decode_err(entry))?;
		let value = kv.value().decode_as::<V>().map_err(decode_err(entry))?;
		out.push((key, value));
	}
	Ok(out)
}
