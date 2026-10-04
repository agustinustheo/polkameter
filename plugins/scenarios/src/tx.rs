//! People's v5 general transactions, built by hand: one layout for every tx we send (ring-proof
//! claims, signed coin transfers, sudo setup calls). Pure bytes, no IO.
//!
//! Wire format: `compact(len) ++ 0x45 ++ ext_version ++ explicit(ext_0) ++ ... ++ explicit(ext_n) ++ call`.
//! An extension that authorizes the tx (VerifySignature, AsResources, ...) signs or proves the
//! *inherited implication*: `blake2_256(ext_version ++ call ++ explicit and implicit data of every
//! later extension)`.
//!
//! The layout is fixed here and checked against the live metadata at startup
//! ([`EXTENSIONS`] vs `extrinsic.signedExtensions`): a runtime with another list needs another
//! encoding, so a mismatch is a tool error, not a chain result.

use parity_scale_codec::{Compact, Encode};
use sp_crypto_hashing::blake2_256;

const GENERAL_V5: u8 = 0b0100_0101;
const EXTENSION_VERSION: u8 = 0;

/// What an extension adds to the signed data beyond its explicit bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Implicit {
	None,
	SpecVersion,
	TxVersion,
	Genesis,
	EraBirth,
}

/// People's transaction extensions, in metadata order (runtime `lib.rs` `TxExtension`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[allow(missing_docs)]
pub enum Ext {
	UnitTransactionExtension,
	VerifyMultiSignature,
	AsPerson,
	AsProofOfInkParticipant,
	ScoreAsParticipant,
	GameAsInvited,
	PeopleLiteAuth,
	AsMember,
	AsCoinage,
	AsResources,
	HonourAuth,
	AuthorizeCall,
	RestrictOrigins,
	CheckNonZeroSender,
	CheckSpecVersion,
	CheckTxVersion,
	CheckGenesis,
	CheckMortality,
	CheckNonce,
	CheckWeight,
	ChargeAssetTxPayment,
	StorageWeightReclaim,
}

/// Each extension: metadata identifier, explicit bytes of its "nothing" value, implicit data.
const SLOTS: [(Ext, &str, &[u8], Implicit); 22] = [
	(Ext::UnitTransactionExtension, "UnitTransactionExtension", &[], Implicit::None),
	(Ext::VerifyMultiSignature, "VerifyMultiSignature", &[0], Implicit::None), // Disabled
	(Ext::AsPerson, "AsPerson", &[0], Implicit::None),
	(Ext::AsProofOfInkParticipant, "AsProofOfInkParticipant", &[0], Implicit::None),
	(Ext::ScoreAsParticipant, "ScoreAsParticipant", &[0], Implicit::None),
	(Ext::GameAsInvited, "GameAsInvited", &[0], Implicit::None),
	(Ext::PeopleLiteAuth, "PeopleLiteAuth", &[0], Implicit::None),
	(Ext::AsMember, "AsMember", &[0], Implicit::None),
	(Ext::AsCoinage, "AsCoinage", &[0], Implicit::None),
	(Ext::AsResources, "AsResources", &[0], Implicit::None),
	(Ext::HonourAuth, "HonourAuth", &[0], Implicit::None),
	(Ext::AuthorizeCall, "AuthorizeCall", &[], Implicit::None),
	(Ext::RestrictOrigins, "RestrictOrigins", &[0], Implicit::None), // false
	(Ext::CheckNonZeroSender, "CheckNonZeroSender", &[], Implicit::None),
	(Ext::CheckSpecVersion, "CheckSpecVersion", &[], Implicit::SpecVersion),
	(Ext::CheckTxVersion, "CheckTxVersion", &[], Implicit::TxVersion),
	(Ext::CheckGenesis, "CheckGenesis", &[], Implicit::Genesis),
	(Ext::CheckMortality, "CheckMortality", &[0], Implicit::EraBirth), // Immortal
	(Ext::CheckNonce, "CheckNonce", &[0], Implicit::None),             // Compact(0)
	(Ext::CheckWeight, "CheckWeight", &[], Implicit::None),
	(Ext::ChargeAssetTxPayment, "ChargeAssetTxPayment", &[0, 0], Implicit::None), // tip 0, asset None
	(Ext::StorageWeightReclaim, "StorageWeightReclaim", &[], Implicit::None),
];

/// The extension identifiers this layout expects, in order.
pub fn expected_extensions() -> impl Iterator<Item = &'static str> {
	SLOTS.iter().map(|s| s.1)
}

pub use polkameter_chain::ChainInfo;

/// One general tx: the call plus explicit values for the extensions it uses.
#[derive(Debug, Clone)]
pub struct GeneralTx<'a> {
	chain: &'a ChainInfo,
	call: Vec<u8>,
	explicit: [Option<Vec<u8>>; 22],
}

impl<'a> GeneralTx<'a> {
	/// A tx for `call` with every extension at its "nothing" value, immortal.
	pub fn new(chain: &'a ChainInfo, call: Vec<u8>) -> Self {
		Self { chain, call, explicit: Default::default() }
	}

	/// Sets the explicit bytes of one extension, e.g. `AsResources` with a proof.
	#[must_use]
	pub fn with(mut self, ext: Ext, explicit: Vec<u8>) -> Self {
		self.explicit[ext as usize] = Some(explicit);
		self
	}

	/// Sets the nonce (for a signed origin).
	#[must_use]
	pub fn nonce(self, nonce: u32) -> Self {
		self.with(Ext::CheckNonce, Compact(nonce).encode())
	}

	fn explicit_of(&self, i: usize) -> &[u8] {
		self.explicit[i].as_deref().unwrap_or(SLOTS[i].2)
	}

	fn implicit_of(&self, i: usize) -> Vec<u8> {
		match SLOTS[i].3 {
			Implicit::None => Vec::new(),
			Implicit::SpecVersion => self.chain.spec_version.to_le_bytes().to_vec(),
			Implicit::TxVersion => self.chain.tx_version.to_le_bytes().to_vec(),
			Implicit::Genesis => self.chain.genesis.to_vec(),
			Implicit::EraBirth => self.chain.genesis.to_vec(),
		}
	}

	/// The message an extension signs or proves: `blake2_256` of the inherited implication,
	/// the data of every extension after `ext`.
	pub fn message_after(&self, ext: Ext) -> [u8; 32] {
		let later = (ext as usize + 1)..SLOTS.len();
		let mut data = vec![EXTENSION_VERSION];
		data.extend_from_slice(&self.call);
		for i in later.clone() {
			data.extend_from_slice(self.explicit_of(i));
		}
		for i in later {
			data.extend(self.implicit_of(i));
		}
		blake2_256(&data)
	}

	/// The tx as sent: `compact(len) ++ body`.
	pub fn encode(&self) -> Vec<u8> {
		let mut body = vec![GENERAL_V5, EXTENSION_VERSION];
		for i in 0..SLOTS.len() {
			body.extend_from_slice(self.explicit_of(i));
		}
		body.extend_from_slice(&self.call);
		let mut out = Compact(body.len() as u32).encode();
		out.extend(body);
		out
	}
}
