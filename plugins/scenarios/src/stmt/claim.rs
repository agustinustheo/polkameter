//! Statement-store slot claims: `Resources.set_statement_store_account(period, seq, account)`
//! from an anonymous person, proved with a ring-VRF proof in
//! `AsResources(Some(RegisterStatementStoreAllowance(proof, ring, revision, People)))`.
//!
//! Same tx as the apps (polkadot-android-community `RegisterStatementStoreAllowance.kt`):
//! - context: `blake2_256("product/peopl.<suffix>/" ++ ("sys/" ++ u32le(2) ++ u32le(period) ++ u32le(seq), padded to 32))`
//! - message: the inherited implication of `AsResources`
//! - period: unix seconds / 86400

use crate::tx::{ChainInfo, Ext, GeneralTx};
use parity_scale_codec::Encode;
use sp_crypto_hashing::blake2_256;

const RESOURCES_PALLET: u8 = 63;
const SET_STATEMENT_STORE_ACCOUNT: u8 = 10;
const STATEMENT_STORE_SLOT_FAMILY: u32 = 2;
const REGISTER_STATEMENT_STORE_ALLOWANCE: u8 = 2;
const COLLECTION_PEOPLE: u8 = 0;

/// The UTC day of a unix time in ms.
pub fn period_of(unix_ms: u64) -> u32 {
	u32::try_from(unix_ms / 1000 / 86_400).expect("days since 1970 fit in u32")
}

/// The proof context of slot `seq` in `period`.
pub fn context(network_suffix: &str, period: u32, seq: u32) -> [u8; 32] {
	let mut slot = [0u8; 32];
	slot[..4].copy_from_slice(b"sys/");
	slot[4..8].copy_from_slice(&STATEMENT_STORE_SLOT_FAMILY.to_le_bytes());
	slot[8..12].copy_from_slice(&period.to_le_bytes());
	slot[12..16].copy_from_slice(&seq.to_le_bytes());
	let mut input = format!("product/peopl.{network_suffix}/").into_bytes();
	input.extend_from_slice(&slot);
	blake2_256(&input)
}

/// `Resources.set_statement_store_account(period, seq, target)`.
pub fn call(period: u32, seq: u32, target: &[u8; 32]) -> Vec<u8> {
	(RESOURCES_PALLET, SET_STATEMENT_STORE_ACCOUNT, period, seq, target).encode()
}

/// A claim before its proof: the tx and the message the proof signs.
#[derive(Debug, Clone)]
pub struct Unproved<'a> {
	tx: GeneralTx<'a>,
}

impl<'a> Unproved<'a> {
	/// A claim of slot `seq` in `period` for `target`.
	pub fn new(chain: &'a ChainInfo, period: u32, seq: u32, target: &[u8; 32]) -> Self {
		Self { tx: GeneralTx::new(chain, call(period, seq, target)) }
	}

	/// What the ring proof signs.
	pub fn message(&self) -> [u8; 32] {
		self.tx.message_after(Ext::AsResources)
	}

	/// The tx with its proof attached.
	pub fn with_proof(self, proof: &[u8], ring_index: u32, revision: u32) -> Vec<u8> {
		// Some(RegisterStatementStoreAllowance { proof, ring_index, revision, collection: People })
		let ext = (
			Some(REGISTER_STATEMENT_STORE_ALLOWANCE),
			proof,
			ring_index,
			revision,
			COLLECTION_PEOPLE,
		)
			.encode();
		self.tx.with(Ext::AsResources, ext).encode()
	}
}
