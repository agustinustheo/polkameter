//! Runtime identity and transaction hashing shared by transports.
/// What the chain adds to every signed message; read over RPC at startup, never hardcoded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChainInfo {
	/// `specVersion` of the running runtime.
	pub spec_version: u32,
	/// `transactionVersion`.
	pub tx_version: u32,
	/// Genesis hash.
	pub genesis: [u8; 32],
}

/// Hash of an encoded extrinsic.
pub fn tx_hash(bytes: &[u8]) -> [u8; 32] {
	sp_crypto_hashing::blake2_256(bytes)
}
