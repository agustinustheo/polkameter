//! Our own people for ring-proof load, made from a run seed, recognized with sudo and put into
//! People rings by the chain (onboard + build_ring).

use polkameter_proofs::{Member, member_key};
use sp_crypto_hashing::blake2_256;

/// `pop:polkadot.network/people` padded with spaces: the People collection id.
pub const PEOPLE_COLLECTION: [u8; 32] = *b"pop:polkadot.network/people     ";
/// People rings are R2e9 (up to 255 keys).
pub const PEOPLE_EXPONENT: u8 = 9;

/// One of our people.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Person {
	/// Index in the run.
	pub index: u32,
	/// Bandersnatch entropy.
	pub entropy: [u8; 32],
	/// Member key.
	pub key: Member,
}

/// Person `index` of a run: `entropy = blake2_256(run_seed ++ "ring" ++ u32le(index))`.
pub fn make_people(run_seed: &[u8; 32], count: u32) -> Vec<Person> {
	(0..count)
		.map(|index| {
			let mut input = [0u8; 40];
			input[..32].copy_from_slice(run_seed);
			input[32..36].copy_from_slice(b"ring");
			input[36..].copy_from_slice(&index.to_le_bytes());
			let entropy = blake2_256(&input);
			Person { index, entropy, key: member_key(entropy) }
		})
		.collect()
}
