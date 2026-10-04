//! Records Recycler maintenance at finalized People blocks (outcomes.md, "Recycler maintenance"):
//!
//! - per collection: keys in the onboarding queue, keys in rings but not yet in a built root, and
//!   stale rings;
//! - cleanup work left: ring pages to delete, old roots, suspended keys to remove;
//! - the maintenance calls (Members and Coinage `*_authorized`) in every finalized block, by result;
//! - for keys a load source hands over and a sample of keys queued in `coinage/recycler`
//!   collections, the time from queueing (`queued_at`, or the block of `Members.MemberAdded`)
//!   until a ring build covers the key.
//!
//! Every finalized block is walked for the calls and ring builds; the storage is read once per
//! finalized update (see walker.rs).

use std::collections::{BTreeMap, HashMap, HashSet};

use polkameter_chain::value::{as_bytes32, as_u64, field, variant_name};
use polkameter_chain::{ChainError, Client, DecodeAsType, Value, calls, entries, events, fetch};
use polkameter_files::registry::{
	CLEANUP_BACKLOG, MAINTENANCE_CALLS, RECYCLER_QUEUED, RECYCLER_STALE, RECYCLER_UNBUILT,
	VOUCHER_IN_ROOT,
};
use polkameter_files::{Millis, Problems, now_ms};
use tokio::sync::watch;

use crate::chain_series::ChainSeries;
use crate::walker::Walk;

const MAINTENANCE_PALLETS: [&str; 2] = ["Members", "Coinage"];
const VOUCHER_COLLECTION: &str = "coinage/recycler";
/// A sampled key not in a built root after this long is dropped, so it doesn't hold a sample slot.
const MAX_TRACK_S: f64 = 3600.0;
const MAX_TRACKED: usize = 200;
const BUILDS_KEPT: usize = 64;

/// A Members collection id.
pub type Id = [u8; 32];
/// A member key.
pub type Key = [u8; 32];

#[derive(DecodeAsType)]
#[decode_as_type(crate_path = "polkameter_chain::scale_decode_reexport")]
struct RingStatus {
	total: u32,
	included: u32,
}

#[derive(Debug, Default)]
struct Tracked {
	/// Seconds since the Unix epoch, from `RingPosition::Onboarding` or `MemberAdded`.
	queued_at: Option<f64>,
	ring: Option<u32>,
	position: Option<u32>,
}

/// "coinage/recycler:0x…" for an id that starts with an ASCII name, else the hex.
pub fn collection_label(id: &Id) -> String {
	let n = id.iter().take_while(|b| (0x20..0x7f).contains(*b)).count();
	let name = String::from_utf8_lossy(&id[..n]).trim().to_owned();
	if n == id.len() {
		name
	} else if n >= 4 {
		format!("{name}:0x{}", hex::encode(&id[n..]))
	} else {
		format!("0x{}", hex::encode(id))
	}
}

/// The Recycler recorder.
pub struct RecyclerRecorder {
	client: Client,
	series: ChainSeries,
	tracked: BTreeMap<(Id, Key), Tracked>,
	/// Recent ring builds per collection and ring: block timestamp and keys included after it.
	builds: HashMap<(Id, u32), Vec<(Millis, u32)>>,
	/// Extrinsics subxt could not name.
	undecoded: u32,
	/// Queued plus unbuilt keys over all collections: at the first and the latest state read.
	backlog: Option<(u64, u64)>,
	drained: watch::Sender<bool>,
}

impl RecyclerRecorder {
	/// A recorder of `client`'s finalized blocks into `series`, following `vouchers` (a
	/// collection and its keys) to a built root. `None` when the chain has no Members pallet;
	/// nothing is recorded then. The receiver says when the backlog is back to where it started.
	pub async fn new(
		client: Client,
		series: ChainSeries,
		vouchers: Option<(Id, Vec<Key>)>,
	) -> Result<Option<(Self, watch::Receiver<bool>)>, ChainError> {
		let at = client.finalized().await?;
		if entries::<(Id, u32), Value>(&at, "Members", "StaleRings").await.is_err() {
			eprintln!(
				"recycler: the chain has no Members pallet; no Recycler maintenance recorded"
			);
			return Ok(None);
		}
		let (drained, rx) = watch::channel(true);
		let mut r = Self {
			client,
			series,
			tracked: BTreeMap::new(),
			builds: HashMap::new(),
			undecoded: 0,
			backlog: None,
			drained,
		};
		if let Some((collection, keys)) = vouchers {
			for key in keys {
				r.tracked.insert((collection, key), Tracked::default());
			}
		}
		Ok(Some((r, rx)))
	}

	/// Observes every sampled key that a recorded ring build now covers.
	fn resolve_built(&mut self) {
		let now_s = now_ms() as f64 / 1000.0;
		let builds = &self.builds;
		let mut observed = Vec::new();
		self.tracked.retain(|(id, _), v| {
			let (Some(ring), Some(position)) = (v.ring, v.position) else { return true };
			// First seen already in a ring: its queue time is unknown.
			let Some(queued_at) = v.queued_at else { return false };
			// The backlog gauges show this one; the sample slot goes to a new key.
			if now_s - queued_at > MAX_TRACK_S {
				return false;
			}
			let Some((built_at, _)) = builds
				.get(&(*id, ring))
				.and_then(|b| b.iter().find(|(_, included)| *included > position))
			else {
				return true;
			};
			observed.push((*built_at as f64 / 1000.0 - queued_at).max(0.0));
			false
		});
		for s in observed {
			self.series.observe(&VOUCHER_IN_ROOT, [], s, now_ms());
		}
	}
}

impl Walk for RecyclerRecorder {
	const NAME: &'static str = "recycler recorder";

	/// Counts the maintenance calls in one block and notes its ring builds.
	async fn on_block(&mut self, _number: u32, hash: [u8; 32]) -> Result<(), ChainError> {
		let at = self.client.at(hash).await?;
		let (calls, events, now) =
			tokio::join!(calls(&at), events(&at), fetch::<(), u64>(&at, "Timestamp", "Now", ()));
		let (calls, events, t) = (calls?, events?, now?.unwrap_or(0));
		let failed: HashSet<u32> = events
			.iter()
			.filter(|e| e.0 == "System" && e.1 == "ExtrinsicFailed")
			.filter_map(|e| e.2)
			.collect();
		for (i, c) in calls.iter().enumerate() {
			let Some((pallet, call)) = c else {
				self.undecoded += 1;
				continue;
			};
			if MAINTENANCE_PALLETS.contains(&pallet.as_str()) && call.ends_with("_authorized") {
				let result = if failed.contains(&(i as u32)) { "failed" } else { "success" };
				self.series.inc(
					&MAINTENANCE_CALLS,
					[&format!("{pallet}.{call}"), result],
					1.0,
					now_ms(),
				);
			}
		}
		// A key can be queued and onboarded within one block; MemberAdded gives its queue time then.
		let added: HashSet<Key> = events
			.iter()
			.filter(|e| e.0 == "Members" && e.1 == "MemberAdded")
			.filter_map(|e| field(&e.3, "key").and_then(as_bytes32))
			.collect();
		if !added.is_empty() {
			for ((_, key), v) in &mut self.tracked {
				if v.queued_at.is_none() && added.contains(key) {
					v.queued_at = Some(t as f64 / 1000.0);
				}
			}
		}
		for e in events.iter().filter(|e| e.0 == "Members" && e.1 == "RingBuilt") {
			let (Some(id), Some(ring)) = (
				field(&e.3, "identifier").and_then(as_bytes32),
				field(&e.3, "ring_index").and_then(as_u64),
			) else {
				continue;
			};
			let ring = ring as u32;
			let status: Option<RingStatus> =
				fetch(&at, "Members", "RingKeysStatus", (id, ring)).await?;
			let list = self.builds.entry((id, ring)).or_default();
			list.push((t, status.map_or(0, |s| s.included)));
			if list.len() > BUILDS_KEPT {
				list.drain(..list.len() - BUILDS_KEPT);
			}
		}
		self.resolve_built();
		Ok(())
	}

	/// Reads the backlog gauges at one finalized block and follows the sampled keys.
	async fn on_update(&mut self, _number: u32, hash: [u8; 32]) -> Result<(), ChainError> {
		let at = self.client.at(hash).await?;
		let (collections, queue, status, stale, deletions, old_roots, suspensions) = tokio::join!(
			entries::<(Id,), Value>(&at, "Members", "Collections"),
			entries::<(Id, u32), Vec<Key>>(&at, "Members", "OnboardingQueue"),
			entries::<(Id, u32), RingStatus>(&at, "Members", "RingKeysStatus"),
			entries::<(Id, u32), Value>(&at, "Members", "StaleRings"),
			entries::<(Id, u32, u32), Value>(&at, "Members", "RingDeletionQueue"),
			entries::<(Id, u32, Value), Value>(&at, "Members", "OldRoots"),
			entries::<(Id, u32), Vec<u32>>(&at, "Members", "PendingSuspensions"),
		);
		let (collections, queue, status, stale, deletions, old_roots, suspensions) =
			(collections?, queue?, status?, stale?, deletions?, old_roots?, suspensions?);
		let now = now_ms();
		// Per collection: queued, unbuilt, stale.
		let mut per: BTreeMap<Id, [u64; 3]> = BTreeMap::new();
		for ((id,), _) in &collections {
			per.entry(*id).or_default();
		}
		for ((id, _), keys) in &queue {
			per.entry(*id).or_default()[0] += keys.len() as u64;
		}
		for ((id, _), s) in &status {
			per.entry(*id).or_default()[1] += u64::from(s.total.saturating_sub(s.included));
		}
		for ((id, _), _) in &stale {
			per.entry(*id).or_default()[2] += 1;
		}
		let total: u64 = per.values().map(|v| v[0] + v[1]).sum();
		let first = self.backlog.map_or(total, |b| b.0);
		self.backlog = Some((first, total));
		let _ = self.drained.send(total <= first);
		for (id, [queued, unbuilt, stale]) in &per {
			let label = collection_label(id);
			self.series.gauge(&RECYCLER_QUEUED, [&label], *queued as f64, now);
			self.series.gauge(&RECYCLER_UNBUILT, [&label], *unbuilt as f64, now);
			self.series.gauge(&RECYCLER_STALE, [&label], *stale as f64, now);
		}
		let suspended: usize = suspensions.iter().map(|(_, v)| v.len()).sum();
		self.series.gauge(&CLEANUP_BACKLOG, ["ring_pages"], deletions.len() as f64, now);
		self.series.gauge(&CLEANUP_BACKLOG, ["old_roots"], old_roots.len() as f64, now);
		self.series.gauge(&CLEANUP_BACKLOG, ["suspensions"], suspended as f64, now);

		// Sample new keys from the voucher queues, then read where every sampled key is now.
		for ((id, _), keys) in &queue {
			if !collection_label(id).starts_with(VOUCHER_COLLECTION) {
				continue;
			}
			for key in keys {
				if self.tracked.len() >= MAX_TRACKED {
					break;
				}
				self.tracked.entry((*id, *key)).or_default();
			}
		}
		let open: Vec<(Id, Key)> = self
			.tracked
			.iter()
			.filter(|(_, v)| v.position.is_none())
			.map(|(k, _)| *k)
			.collect();
		for (id, key) in open {
			let pos: Option<Value> = fetch(&at, "Members", "Members", (id, key)).await?;
			let v = self.tracked.get_mut(&(id, key)).expect("listed");
			let number = |name: &str| pos.as_ref().and_then(|p| field(p, name)).and_then(as_u64);
			match pos.as_ref().and_then(variant_name) {
				Some("Onboarding") => {
					if v.queued_at.is_none() {
						v.queued_at = number("queued_at").map(|s| s as f64);
					}
				},
				Some("Included") => {
					v.ring = number("ring_index").map(|n| n as u32);
					v.position = number("ring_position").map(|n| n as u32);
				},
				_ => {
					self.tracked.remove(&(id, key)); // suspended or gone
				},
			}
		}
		self.resolve_built();
		Ok(())
	}

	fn finish(&mut self, problems: &Problems) {
		if self.undecoded > 0 {
			problems.record(format!(
				"recycler recorder: {} extrinsics could not be named; maintenance calls may be undercounted",
				self.undecoded
			));
		}
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn collection_labels_read_as_the_ts_tool_writes_them() {
		let mut id = [0u8; 32];
		id[..16].copy_from_slice(b"coinage/paidtkn!");
		id[16] = 0xfa;
		id[17] = 0x1a;
		assert_eq!(collection_label(&id), "coinage/paidtkn!:0xfa1a0000000000000000000000000000");
		assert_eq!(collection_label(&[b'a'; 32]), "a".repeat(32));
		assert_eq!(collection_label(&[1u8; 32]), format!("0x{}", "01".repeat(32)));
		let mut spaced = [b' '; 32];
		spaced[..6].copy_from_slice(b"people");
		assert_eq!(collection_label(&spaced), "people");
	}
}
