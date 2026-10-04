//! Built rings of a Members collection, read as the apps read them: the ring's keys in
//! insertion order (`Members.RingKeys` pages), cut to `RingKeysStatus.included`, which is what
//! the root in `Members.Root` covers. A ring-VRF proof is made against these keys.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use polkameter_chain::{AtBlock, ChainError, Client, DecodeAsType, Value, fetch};
use polkameter_proofs::Member;

/// A built ring.
#[derive(Debug, Clone)]
pub struct Ring {
	/// Ring index in the collection.
	pub index: u32,
	/// `Members.Root.revision`.
	pub revision: u32,
	/// The keys the root covers.
	pub keys: Vec<Member>,
}

#[derive(DecodeAsType)]
#[decode_as_type(crate_path = "polkameter_chain::scale_decode_reexport")]
struct Status {
	included: u32,
}

#[derive(DecodeAsType)]
#[decode_as_type(crate_path = "polkameter_chain::scale_decode_reexport")]
struct Root {
	revision: u32,
}

/// Ring `index` of `collection` as built at `at`; `None` before its first root.
pub async fn read_ring(
	at: &AtBlock,
	collection: [u8; 32],
	index: u32,
) -> Result<Option<Ring>, ChainError> {
	let status: Option<Status> =
		fetch(at, "Members", "RingKeysStatus", (collection, index)).await?;
	let root: Option<Root> = fetch(at, "Members", "Root", (collection, index)).await?;
	let (Some(status), Some(root)) = (status, root) else { return Ok(None) };
	let mut keys = Vec::new();
	for page in 0u32.. {
		let page_keys: Option<Vec<Member>> =
			fetch(at, "Members", "RingKeys", (collection, index, page)).await?;
		match page_keys {
			Some(k) if !k.is_empty() && keys.len() < status.included as usize => keys.extend(k),
			_ => break,
		}
	}
	keys.truncate(status.included as usize);
	Ok(Some(Ring { index, revision: root.revision, keys }))
}

/// The ring a key is in (`Members.Members` = `Included { ring_index, .. }`), if any.
async fn ring_of(
	at: &AtBlock,
	collection: [u8; 32],
	key: Member,
) -> Result<Option<u32>, ChainError> {
	let pos: Option<Value> = fetch(at, "Members", "Members", (collection, key)).await?;
	Ok(pos.and_then(|v| included_ring(&v)))
}

fn included_ring(v: &Value) -> Option<u32> {
	use polkameter_chain::scale_value_reexport::{Composite, ValueDef};
	let ValueDef::Variant(var) = &v.value else { return None };
	let Composite::Named(fields) = &var.values else { return None };
	(var.name == "Included")
		.then(|| fields.iter().find(|(k, _)| k == "ring_index")?.1.as_u128()?.try_into().ok())
		.flatten()
}

/// Waits until every key is in a built ring root of `collection` at the finalized block, so the
/// setup is settled before the load. Returns the rings by index and each key's ring.
pub async fn wait_for_rings(
	client: &Client,
	collection: [u8; 32],
	keys: &[Member],
	timeout: Duration,
) -> Result<(BTreeMap<u32, Ring>, Vec<u32>), ChainError> {
	let deadline = Instant::now() + timeout;
	loop {
		let at = client.finalized().await?;
		let mut ring_of_key = Vec::with_capacity(keys.len());
		for key in keys {
			ring_of_key.push(ring_of(&at, collection, *key).await?);
		}
		let placed = ring_of_key.iter().flatten().count();
		if placed == keys.len() {
			let index: Vec<u32> = ring_of_key.iter().map(|r| r.expect("all placed")).collect();
			let mut rings = BTreeMap::new();
			for i in index.iter().copied().collect::<std::collections::BTreeSet<_>>() {
				if let Some(ring) = read_ring(&at, collection, i).await? {
					rings.insert(i, ring);
				}
			}
			let covered = keys
				.iter()
				.zip(&index)
				.all(|(k, i)| rings.get(i).is_some_and(|r| r.keys.contains(k)));
			if covered {
				return Ok((rings, index));
			}
		}
		if Instant::now() > deadline {
			return Err(ChainError::Timeout(format!(
				"{placed} of {} keys in built rings after {timeout:?}",
				keys.len()
			)));
		}
		eprintln!("rings: {placed} of {} keys in a ring, waiting for ring roots", keys.len());
		tokio::time::sleep(Duration::from_secs(6)).await;
	}
}
