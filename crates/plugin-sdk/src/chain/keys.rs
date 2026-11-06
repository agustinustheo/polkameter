//! Signers: sr25519 keys from secret URIs, and many test accounts derived from one seed.
//!
//! A secret URI is what `subkey` and polkadot.js accept: `//Alice` (the dev phrase with a hard
//! junction), `<mnemonic>//stress//0`, `0x<32-byte seed hex>//0`, with an optional `///password`
//! at the end. The functions here never put the URI, or any part of it, in an error.

use std::ops::Range;

use subxt_signer::{DeriveJunction, SecretUri, sr25519::Keypair};

/// A secret URI, derivation path or account that cannot be used. The message never contains the
/// secret.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum KeyError {
	/// The secret URI does not parse, or its phrase or seed is not valid.
	#[error("the secret URI is not valid ({0}); expected //Name, <mnemonic>//path or 0x<seed>")]
	Uri(&'static str),
	/// A derivation path is not `//hard` and `/soft` junctions.
	#[error("derivation path {path:?} is not valid: {reason}")]
	Path {
		/// The path as given; paths hold no secret.
		path: String,
		/// Why.
		reason: &'static str,
	},
	/// An account is neither SS58 nor `0x` and 64 hex digits.
	#[error("account {0:?} is not valid: expected an SS58 address or 0x and 64 hex digits")]
	Account(String),
}

/// The sr25519 keypair of a secret URI, such as `//Alice` or `<mnemonic>//0`.
pub fn keypair(uri: &str) -> Result<Keypair, KeyError> {
	let uri: SecretUri = uri.trim().parse().map_err(|_| KeyError::Uri("unrecognised format"))?;
	Keypair::from_uri(&uri).map_err(|_| KeyError::Uri("bad phrase or seed"))
}

/// The junctions of a derivation path: `//name` is hard, `/name` is soft, and a number is encoded
/// as a number, as `subkey` does. The empty path has no junctions.
pub fn junctions(path: &str) -> Result<Vec<DeriveJunction>, KeyError> {
	let bad = |reason| KeyError::Path { path: path.to_owned(), reason };
	if path.is_empty() {
		return Ok(Vec::new());
	}
	let mut out = Vec::new();
	let mut chars = path;
	while !chars.is_empty() {
		let (is_hard, after) = match chars.strip_prefix("//") {
			Some(after) => (true, after),
			None => (false, chars.strip_prefix('/').ok_or_else(|| bad("expected /"))?),
		};
		let end = after.find('/').unwrap_or(after.len());
		let name = &after[..end];
		if name.is_empty() {
			return Err(bad("a junction is empty or has more than two slashes"));
		}
		out.push(DeriveJunction::from(if is_hard { format!("/{name}") } else { name.to_owned() }));
		chars = &after[end..];
	}
	Ok(out)
}

/// `base` derived along `path`, such as `//polkameter//0`.
pub fn derive(base: &Keypair, path: &str) -> Result<Keypair, KeyError> {
	Ok(base.derive(junctions(path)?))
}

/// One keypair per index in `indices`, each `base` derived along `prefix` followed by the index:
/// with prefix `//load//` and indices `0..3`, the paths are `//load//0`, `//load//1` and
/// `//load//2`. The prefix must end with `/` or `//`.
pub fn derive_range(
	base: &Keypair,
	prefix: &str,
	indices: Range<u32>,
) -> Result<Vec<Keypair>, KeyError> {
	if !prefix.ends_with('/') {
		return Err(KeyError::Path {
			path: prefix.to_owned(),
			reason: "a prefix must end with / or //",
		});
	}
	let fixed = junctions(prefix.trim_end_matches('/'))?;
	let hard = prefix.ends_with("//");
	Ok(indices
		.map(|i| {
			let last = DeriveJunction::soft(u64::from(i));
			let last = if hard { last.harden() } else { last };
			base.derive(fixed.iter().copied().chain([last]))
		})
		.collect())
}

/// The 32-byte account ID of an sr25519 keypair: its public key.
pub fn account_id(keypair: &Keypair) -> [u8; 32] {
	keypair.public_key().0
}

/// The SS58 address of `account`, with the generic prefix 42.
pub fn ss58(account: [u8; 32]) -> String {
	subxt::utils::AccountId32(account).to_string()
}

/// An account from an SS58 address (any prefix) or `0x` and 64 hex digits.
pub fn parse_account(text: &str) -> Result<[u8; 32], KeyError> {
	let text = text.trim();
	if let Some(hex) = text.strip_prefix("0x") {
		return hex::decode(hex)
			.ok()
			.and_then(|bytes| bytes.try_into().ok())
			.ok_or_else(|| KeyError::Account(text.to_owned()));
	}
	text.parse::<subxt::utils::AccountId32>()
		.map(|account| account.0)
		.map_err(|_| KeyError::Account(text.to_owned()))
}

#[cfg(test)]
mod tests {
	use super::*;

	const ALICE: &str = "5GrwvaEF5zXb26Fz9rcQpDWS57CtERHpNehXCPcNoHGKutQY";
	const BOB: &str = "5FHneW46xGXgs5mUiveU4sbTyGBzmstUspZC92UhjJM694ty";

	#[test]
	fn dev_uris_give_the_well_known_accounts() {
		assert_eq!(ss58(account_id(&keypair("//Alice").unwrap())), ALICE);
		assert_eq!(ss58(account_id(&keypair(" //Bob ").unwrap())), BOB);
		// The dev phrase spelled out is the same root.
		let spelled = format!("{}//Alice", subxt_signer::DEV_PHRASE);
		assert_eq!(
			account_id(&keypair(&spelled).unwrap()),
			account_id(&keypair("//Alice").unwrap())
		);
	}

	#[test]
	fn derived_accounts_match_the_uri_form() {
		let root = keypair(subxt_signer::DEV_PHRASE).unwrap();
		let derived = derive_range(&root, "//polkameter//", 0..3).unwrap();
		for (i, key) in derived.iter().enumerate() {
			let direct =
				keypair(&format!("{}//polkameter//{i}", subxt_signer::DEV_PHRASE)).unwrap();
			assert_eq!(account_id(key), account_id(&direct));
		}
		let alice = keypair("//Alice").unwrap();
		let soft = derive_range(&alice, "/load/", 5..6).unwrap();
		assert_eq!(account_id(&soft[0]), account_id(&keypair("//Alice/load/5").unwrap()));
		assert_eq!(
			account_id(&derive(&alice, "//a/b//7").unwrap()),
			account_id(&keypair("//Alice//a/b//7").unwrap())
		);
		assert_eq!(account_id(&derive(&alice, "").unwrap()), account_id(&alice));
	}

	#[test]
	fn derived_accounts_are_distinct() {
		let root = keypair("//Alice").unwrap();
		let keys = derive_range(&root, "//x//", 0..50).unwrap();
		let unique: std::collections::HashSet<_> = keys.iter().map(account_id).collect();
		assert_eq!(unique.len(), 50);
	}

	#[test]
	fn bad_input_is_refused_without_echoing_secrets() {
		let secret = "not a valid mnemonic at all//0";
		let error = keypair(secret).unwrap_err().to_string();
		assert!(!error.contains("mnemonic at all"), "{error}");
		for path in ["x", "///x", "//", "//a///b", "/a//"] {
			assert!(junctions(path).is_err(), "{path}");
		}
		assert!(derive_range(&keypair("//Alice").unwrap(), "//x", 0..1).is_err());
	}

	#[test]
	fn accounts_parse_from_ss58_and_hex() {
		let alice = account_id(&keypair("//Alice").unwrap());
		assert_eq!(parse_account(ALICE).unwrap(), alice);
		assert_eq!(parse_account(&format!("0x{}", hex::encode(alice))).unwrap(), alice);
		assert!(parse_account("0x1234").is_err());
		assert!(parse_account("5Grwva").is_err());
	}
}
