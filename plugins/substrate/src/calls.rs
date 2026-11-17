//! `prepare-calls`, the prepare step of `polkameter calls`: the user supplies only what to send
//! (call data, from a call file or a `Pallet.call` with JSON arguments), and this does the rest:
//! derives and funds the test accounts, runs the call file's setup, and signs every call at
//! explicit, consecutive nonces with a mortality, for any runtime.
//!
//! Signing is generic. The address and signature encodings come from the runtime's metadata:
//! `MultiAddress` or a bare `AccountId32` with an sr25519 `MultiSignature`, or a bare
//! `AccountId20` with a 65-byte Ethereum signature (keccak-256, secp256k1), as on Mythos and
//! Darwinia. Every transaction extension the metadata lists is filled: the standard ones with
//! their values, any other with the default of its types (`None`, the first unit variant, zero),
//! which is the "no-op" value of extensions such as `CheckMetadataHash` or an asset-fee choice.
//! When a default is wrong, the node refuses the first transaction and the error names those
//! extensions.

use std::{
	collections::{HashMap, HashSet},
	path::{Path, PathBuf},
	time::Duration,
};

use anyhow::{Context as _, Result, anyhow, bail, ensure};
use parity_scale_codec::{Compact, Encode};
use polkameter_plugin_sdk::{
	Artifact, Context, PreparedTx,
	chain::{
		AtBlock, Client, Keypair, derive_range, era_period, fetch, hex0x, keypair, ss58, tx_hash,
		value,
	},
};
use scale_info::{PortableRegistry, TypeDef, TypeDefPrimitive};
use serde_json::{Value, json};
use subxt::dynamic::Value as ScaleValue;
use subxt_signer::eth;
use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};

use crate::inputs;

/// The call file runner, written next to the call file so that its imports resolve from the
/// user's `node_modules`.
const RUNNER: &str = include_str!("calls-runner.mjs");

/// A key that signs: sr25519, or Ethereum-style secp256k1.
pub enum Key {
	/// sr25519, for `AccountId32` runtimes.
	Sr(Keypair),
	/// secp256k1 with keccak-256, for `AccountId20` runtimes.
	Eth(eth::Keypair),
}

impl Key {
	/// The account ID: the sr25519 public key, or the last 20 bytes of the keccak-256 of the
	/// uncompressed secp256k1 public key.
	pub fn account(&self) -> Vec<u8> {
		match self {
			Key::Sr(k) => k.public_key().0.to_vec(),
			Key::Eth(k) => sp_crypto_hashing::keccak_256(&k.public_key().0[1..])[12..].to_vec(),
		}
	}

	fn sign(&self, payload: &[u8]) -> Vec<u8> {
		match self {
			Key::Sr(k) => k.sign(payload).0.to_vec(),
			Key::Eth(k) => k.sign(payload).0.to_vec(),
		}
	}
}

/// An account as a call file sees it: SS58 (prefix 42) for 32 bytes, `0x` hex otherwise.
pub fn show(account: &[u8]) -> String {
	match <[u8; 32]>::try_from(account) {
		Ok(account) => ss58(account),
		Err(_) => hex0x(account),
	}
}

/// The key of a seed. sr25519: a secret URI such as `//Alice` or `<mnemonic>//path`. Ethereum:
/// a dev name such as `//Alith`, `0x` and a 32-byte private key, or a BIP-39 mnemonic (first
/// BIP-44 Ethereum account). Errors never show the seed.
pub fn seed_key(uri: &str, eth_chain: bool) -> Result<Key> {
	let uri = uri.trim();
	if !eth_chain {
		return keypair(uri)
			.map(Key::Sr)
			.map_err(|e| anyhow!("input seed is not a valid secret URI: {e}"));
	}
	let bad = || {
		anyhow!(
			"input seed is not a valid Ethereum key: pass //Alith (or another dev name), 0x and a 32-byte private key, or a mnemonic"
		)
	};
	if let Some(name) = uri.strip_prefix("//") {
		use eth::dev;
		let key = match name.to_ascii_lowercase().as_str() {
			"alith" => dev::alith(),
			"baltathar" => dev::baltathar(),
			"charleth" => dev::charleth(),
			"dorothy" => dev::dorothy(),
			"ethan" => dev::ethan(),
			"faith" => dev::faith(),
			_ => return Err(bad()),
		};
		return Ok(Key::Eth(key));
	}
	if let Some(hex) = uri.strip_prefix("0x") {
		let secret: [u8; 32] =
			hex::decode(hex).ok().and_then(|b| b.try_into().ok()).ok_or_else(bad)?;
		return eth::Keypair::from_secret_key(secret).map(Key::Eth).map_err(|_| bad());
	}
	let phrase = subxt_signer::bip39::Mnemonic::parse(uri).map_err(|_| bad())?;
	eth::Keypair::from_phrase(&phrase, None, eth::DerivationPath::eth(0, 0))
		.map(Key::Eth)
		.map_err(|_| bad())
}

/// Test keys `range` of `seed` along `derivation`. sr25519 keys are derived as `subkey` does
/// (`<seed>//polkameter//0`); Ethereum keys from the BLAKE2 hash of the seed's private key, the
/// derivation and the index.
pub fn derive(seed: &Key, derivation: &str, range: std::ops::Range<u32>) -> Result<Vec<Key>> {
	match seed {
		Key::Sr(k) => Ok(derive_range(k, derivation, range)?.into_iter().map(Key::Sr).collect()),
		Key::Eth(k) => range
			.map(|i| {
				let mut material = k.secret_key().to_vec();
				material.extend(derivation.as_bytes());
				material.extend(i.to_le_bytes());
				eth::Keypair::from_secret_key(sp_crypto_hashing::blake2_256(&material))
					.map(Key::Eth)
					.map_err(|_| anyhow!("derived Ethereum key {i} is not valid"))
			})
			.collect(),
	}
}

/// How one part of the transaction is encoded: a variant (its index, then the bytes), or the
/// bytes alone.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Wire {
	Variant(u8),
	Raw,
}

/// The runtime's address and signature encodings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Format {
	address: Wire,
	signature: Wire,
	/// Bytes of an account ID: 32 or 20.
	pub account_len: usize,
	/// Ethereum-style keys sign.
	pub eth: bool,
}

/// The size of a type that is a fixed number of bytes, such as `[u8; 32]` or a struct of one.
fn fixed_len(types: &PortableRegistry, id: u32) -> Option<usize> {
	match &types.resolve(id)?.type_def {
		TypeDef::Array(a) => Some(a.len as usize * fixed_len(types, a.type_param.id)?),
		TypeDef::Composite(c) => c.fields.iter().map(|f| fixed_len(types, f.ty.id)).sum(),
		TypeDef::Tuple(t) => t.fields.iter().map(|f| fixed_len(types, f.id)).sum(),
		TypeDef::Primitive(TypeDefPrimitive::U8) => Some(1),
		_ => None,
	}
}

/// The index and only field type of variant `name` of an enum type.
fn variant(types: &PortableRegistry, id: u32, name: &str) -> Option<(u8, Option<u32>)> {
	match &types.resolve(id)?.type_def {
		TypeDef::Variant(v) => v
			.variants
			.iter()
			.find(|x| x.name == name)
			.map(|x| (x.index, x.fields.first().map(|f| f.ty.id))),
		_ => None,
	}
}

impl Format {
	/// The encodings of the runtime of `metadata`; an error names its types when no key here
	/// can sign for it.
	pub fn of(metadata: &subxt::Metadata) -> Result<Self> {
		use subxt::ext::frame_decode::extrinsics::ExtrinsicTypeInfo as _;
		let info = metadata
			.extrinsic_signature_info()
			.map_err(|e| anyhow!("the runtime's transaction format: {e}"))?;
		let types = metadata.types();
		let (address, account_len) = match variant(types, info.address_id, "Id") {
			Some((index, Some(inner))) => (Wire::Variant(index), fixed_len(types, inner)),
			_ => (Wire::Raw, fixed_len(types, info.address_id)),
		};
		let sr = variant(types, info.signature_id, "Sr25519");
		let format = match (account_len, sr) {
			(Some(32), Some((index, _))) => {
				Self { address, signature: Wire::Variant(index), account_len: 32, eth: false }
			},
			(Some(20), _) if fixed_len(types, info.signature_id) == Some(65) => {
				Self { address, signature: Wire::Raw, account_len: 20, eth: true }
			},
			_ => {
				let name = |id: u32| {
					types
						.resolve(id)
						.and_then(|t| t.path.segments.last().cloned())
						.unwrap_or_default()
				};
				bail!(
					"calls mode cannot sign for this runtime: its address type is {} and its signature type {}; it signs AccountId32 with sr25519 MultiSignature, or AccountId20 with a 65-byte Ethereum signature",
					name(info.address_id),
					name(info.signature_id)
				)
			},
		};
		Ok(format)
	}
}

/// The default value of a type, SCALE-encoded: zero, empty, `None` or the first unit variant.
/// This is the "no-op" value of an optional transaction extension.
fn default_bytes(types: &PortableRegistry, id: u32, out: &mut Vec<u8>) -> Result<()> {
	let ty = types.resolve(id).with_context(|| format!("type {id} is not in the metadata"))?;
	match &ty.type_def {
		TypeDef::Composite(c) => {
			for f in &c.fields {
				default_bytes(types, f.ty.id, out)?;
			}
		},
		TypeDef::Tuple(t) => {
			for f in &t.fields {
				default_bytes(types, f.id, out)?;
			}
		},
		TypeDef::Array(a) => {
			for _ in 0..a.len {
				default_bytes(types, a.type_param.id, out)?;
			}
		},
		TypeDef::Sequence(_) | TypeDef::Compact(_) | TypeDef::BitSequence(_) => out.push(0),
		TypeDef::Primitive(p) => {
			let size = match p {
				TypeDefPrimitive::Bool | TypeDefPrimitive::U8 | TypeDefPrimitive::I8 => 1,
				TypeDefPrimitive::Str => 1,
				TypeDefPrimitive::U16 | TypeDefPrimitive::I16 => 2,
				TypeDefPrimitive::U32 | TypeDefPrimitive::I32 | TypeDefPrimitive::Char => 4,
				TypeDefPrimitive::U64 | TypeDefPrimitive::I64 => 8,
				TypeDefPrimitive::U128 | TypeDefPrimitive::I128 => 16,
				TypeDefPrimitive::U256 | TypeDefPrimitive::I256 => 32,
			};
			out.extend(std::iter::repeat_n(0, size));
		},
		TypeDef::Variant(v) => {
			let pick = v
				.variants
				.iter()
				.find(|x| x.name == "None")
				.or_else(|| v.variants.iter().find(|x| x.fields.is_empty()))
				.or_else(|| v.variants.first())
				.with_context(|| format!("{} has no variant", ty.path))?;
			out.push(pick.index);
			for f in &pick.fields {
				default_bytes(types, f.ty.id, out)?;
			}
		},
	}
	Ok(())
}

/// `n` encoded as the integer type `id`: compact, a fixed-width integer, or a struct of one.
fn int_bytes(types: &PortableRegistry, id: u32, n: u64, out: &mut Vec<u8>) -> Result<()> {
	match &types.resolve(id).with_context(|| format!("type {id}"))?.type_def {
		TypeDef::Compact(_) => Compact(n).encode_to(out),
		TypeDef::Primitive(TypeDefPrimitive::U32) => {
			u32::try_from(n).context("nonce above u32")?.encode_to(out)
		},
		TypeDef::Primitive(TypeDefPrimitive::U64) => n.encode_to(out),
		TypeDef::Primitive(TypeDefPrimitive::U128) => u128::from(n).encode_to(out),
		TypeDef::Composite(c) if c.fields.len() == 1 => {
			int_bytes(types, c.fields[0].ty.id, n, out)?
		},
		other => bail!("a nonce of type {other:?} is not supported"),
	}
	Ok(())
}

/// Signs call data for one runtime, against one finalized block, without RPC.
pub struct Signer {
	at: AtBlock,
	/// The address and signature encodings.
	pub format: Format,
	genesis: [u8; 32],
	/// Blocks the transactions stay valid for; `None` for immortal.
	pub period: Option<u64>,
	/// Extensions filled with the default of their types, with data: named in refusals.
	pub defaulted: Vec<String>,
}

impl Signer {
	/// A signer at the node's finalized head, with a mortality of `period` blocks from it.
	pub async fn new(client: &Client, period: Option<u64>) -> Result<Self> {
		let at = client.finalized_now().await?;
		let format = Format::of(at.metadata_ref())?;
		ensure!(
			at.metadata_ref().extrinsic().supported_versions().contains(&4),
			"the runtime does not take version 4 (signed) transactions"
		);
		if let Some(period) = period {
			ensure!(
				era_period(period) <= 4096,
				"a mortality above 4096 blocks is not supported in calls mode"
			);
		}
		let genesis = client.chain_info().await?.genesis;
		let mut signer = Self { at, format, genesis, period, defaulted: Vec::new() };
		let types = signer.at.metadata_ref().types();
		let mut defaulted = Vec::new();
		for e in signer.extension_list()? {
			let (mut extra, mut implicit) = (Vec::new(), Vec::new());
			if !KNOWN.contains(&e.0.as_str()) {
				default_bytes(types, e.1, &mut extra)?;
				default_bytes(types, e.2, &mut implicit)?;
				if !extra.is_empty() || !implicit.is_empty() {
					defaulted.push(e.0);
				}
			}
		}
		signer.defaulted = defaulted;
		Ok(signer)
	}

	/// The block the transactions are signed against.
	pub fn block_number(&self) -> u64 {
		self.at.block_number()
	}

	/// The last block the transactions are valid in, for mortal ones.
	pub fn valid_until(&self) -> Option<u64> {
		self.period.map(|p| self.block_number() + era_period(p) - 1)
	}

	/// The metadata.
	pub fn metadata(&self) -> &subxt::Metadata {
		self.at.metadata_ref()
	}

	/// The block, for encoding calls.
	pub fn at(&self) -> &AtBlock {
		&self.at
	}

	fn extension_list(&self) -> Result<Vec<(String, u32, u32)>> {
		Ok(self
			.metadata()
			.extrinsic()
			.transaction_extensions_by_version(0)
			.context("the runtime lists no transaction extensions for signed transactions")?
			.map(|e| (e.identifier().to_owned(), e.extra_ty(), e.additional_ty()))
			.collect())
	}

	/// The extensions' data in the transaction, and what the signature also covers.
	fn extensions(&self, nonce: u64) -> Result<(Vec<u8>, Vec<u8>)> {
		let types = self.metadata().types();
		let (mut extra, mut implicit) = (Vec::new(), Vec::new());
		for (name, extra_ty, implicit_ty) in self.extension_list()? {
			match name.as_str() {
				"CheckNonce" => int_bytes(types, extra_ty, nonce, &mut extra)?,
				"CheckMortality" | "CheckEra" => match self.period {
					Some(period) => {
						subxt::utils::Era::mortal(period, self.block_number())
							.encode_to(&mut extra);
						implicit.extend(self.at.block_hash().0);
					},
					None => {
						subxt::utils::Era::Immortal.encode_to(&mut extra);
						implicit.extend(self.genesis);
					},
				},
				"CheckGenesis" => implicit.extend(self.genesis),
				"CheckSpecVersion" => self.at.spec_version().encode_to(&mut implicit),
				"CheckTxVersion" => self.at.transaction_version().encode_to(&mut implicit),
				_ => {
					default_bytes(types, extra_ty, &mut extra)?;
					default_bytes(types, implicit_ty, &mut implicit)?;
				},
			}
		}
		Ok((extra, implicit))
	}

	/// `call` (SCALE call data), signed by `key` with `nonce`: the bytes to submit.
	pub fn sign(&self, call: &[u8], key: &Key, nonce: u64) -> Result<Vec<u8>> {
		ensure!(
			matches!(key, Key::Eth(_)) == self.format.eth,
			"the key type does not match the runtime's signature type"
		);
		let (extra, implicit) = self.extensions(nonce)?;
		let mut payload = [call, &extra, &implicit].concat();
		if payload.len() > 256 {
			payload = sp_crypto_hashing::blake2_256(&payload).to_vec();
		}
		let mut body = vec![0x84];
		if let Wire::Variant(index) = self.format.address {
			body.push(index);
		}
		body.extend(key.account());
		if let Wire::Variant(index) = self.format.signature {
			body.push(index);
		}
		body.extend(key.sign(&payload));
		body.extend(extra);
		body.extend(call);
		let mut out = Compact(u32::try_from(body.len())?).encode();
		out.extend(body);
		Ok(out)
	}

	/// The SCALE call data of `pallet.call(fields)`, checked against the metadata.
	pub fn call_data(&self, pallet: &str, call: &str, fields: Vec<ScaleValue>) -> Result<Vec<u8>> {
		self.at
			.transactions()
			.call_data(&subxt::dynamic::tx(pallet, call, fields))
			.map_err(|e| anyhow!("{pallet}.{call}: {e}"))
	}

	/// `Balances.transfer_keep_alive(dest, amount)`, with `dest` as the runtime's address.
	fn transfer(&self, dest: &[u8], amount: u128) -> Result<Vec<u8>> {
		let raw = ScaleValue::from_bytes(dest);
		let multi = ScaleValue::unnamed_variant("Id", [raw.clone()]);
		self.call_data("Balances", "transfer_keep_alive", vec![multi, ScaleValue::u128(amount)])
			.or_else(|_| {
				self.call_data(
					"Balances",
					"transfer_keep_alive",
					vec![raw, ScaleValue::u128(amount)],
				)
			})
	}
}

/// Extensions whose values the signer knows.
const KNOWN: [&str; 6] = [
	"CheckNonce",
	"CheckMortality",
	"CheckEra",
	"CheckGenesis",
	"CheckSpecVersion",
	"CheckTxVersion",
];

/// The next nonce of `account`, with the node's pool.
async fn next_nonce(client: &Client, account: &[u8]) -> Result<u64> {
	Ok(client
		.request("system_accountNextIndex", subxt::rpcs::rpc_params![show(account)])
		.await?)
}

/// `System.Account` of `account` at `at`: its nonce and what it can spend.
async fn account_at(at: &AtBlock, account: &[u8]) -> Result<(u64, u128)> {
	let raw: Option<ScaleValue> = match account.len() {
		32 => fetch(at, "System", "Account", (<[u8; 32]>::try_from(account)?,)).await?,
		20 => fetch(at, "System", "Account", (<[u8; 20]>::try_from(account)?,)).await?,
		n => bail!("an account of {n} bytes is not supported"),
	};
	let Some(raw) = raw else { return Ok((0, 0)) };
	let data = value::field(&raw, "data");
	let balance = |name: &str| data.and_then(|d| value::field(d, name)).and_then(value::as_u128);
	let frozen = balance("frozen")
		.or_else(|| balance("misc_frozen").max(balance("fee_frozen")))
		.unwrap_or(0);
	let nonce = value::field(&raw, "nonce").and_then(value::as_u64).unwrap_or(0);
	Ok((nonce, balance("free").unwrap_or(0).saturating_sub(frozen)))
}

/// A transaction sent before the load, waiting to be final.
struct Pending {
	hash: [u8; 32],
	signer: Vec<u8>,
	nonce: u64,
	/// The first block it can be in.
	from: u64,
	sent: tokio::time::Instant,
}

/// Which of `pending` are final now: `(index, Ok or the failure)`. A transaction is final when
/// its signer's finalized nonce is past it; it failed when its block has `ExtrinsicFailed`.
async fn resolve(
	client: &Client,
	pending: &[Pending],
	timeout: Duration,
) -> Result<Vec<(usize, Result<()>)>> {
	let head = client.finalized_now().await?;
	let mut done = Vec::new();
	for (i, p) in pending.iter().enumerate() {
		if account_at(&head, &p.signer).await?.0 > p.nonce {
			done.push(i);
		} else if p.sent.elapsed() > timeout {
			return Ok(vec![(
				i,
				Err(anyhow!("{} not final after {} s", hex0x(p.hash), timeout.as_secs())),
			)]);
		}
	}
	if done.is_empty() {
		return Ok(Vec::new());
	}
	let ours: HashSet<[u8; 32]> = done.iter().map(|i| pending[*i].hash).collect();
	let from = done.iter().map(|i| pending[*i].from).min().unwrap_or(0);
	let builder = client.tx_builder().await?;
	let failed = crate::failed_funding(client, &builder, from, &head, &ours).await?;
	Ok(done
		.into_iter()
		.map(|i| {
			let hash = hex0x(pending[i].hash);
			match failed.iter().find(|f| f.starts_with(&hash)) {
				Some(f) => (i, Err(anyhow!("failed: {f}"))),
				None => (i, Ok(())),
			}
		})
		.collect())
}

/// Signs and submits `call` from `key` with its next nonce (tracked in `nonces`).
async fn send(
	client: &Client,
	signer: &Signer,
	key: &Key,
	call: &[u8],
	nonces: &mut HashMap<Vec<u8>, u64>,
) -> Result<Pending> {
	let account = key.account();
	let nonce = match nonces.get(&account) {
		Some(n) => *n,
		None => next_nonce(client, &account).await?,
	};
	let bytes = signer.sign(call, key, nonce)?;
	let from = client.finalized_now().await?.block_number() + 1;
	client.submit(&bytes).await.map_err(|e| {
		anyhow!("{}{}", crate::describe(&e, signer.period.is_some()), defaulted_note(signer))
	})?;
	nonces.insert(account.clone(), nonce + 1);
	Ok(Pending {
		hash: tx_hash(&bytes),
		signer: account,
		nonce,
		from,
		sent: tokio::time::Instant::now(),
	})
}

fn defaulted_note(signer: &Signer) -> String {
	if signer.defaulted.is_empty() {
		String::new()
	} else {
		format!(
			"; these transaction extensions were filled with their default value, which may be wrong for this runtime: {}",
			signer.defaulted.join(", ")
		)
	}
}

/// Tops up every account in `accounts` to `amount` spendable from `funder`, and waits until
/// the top-ups are final. Returns how many it topped up.
async fn fund(
	client: &Client,
	signer: &Signer,
	funder: &Key,
	accounts: &[Vec<u8>],
	amount: u128,
	timeout: Duration,
) -> Result<usize> {
	let head = client.finalized_now().await?;
	let mut transfers = Vec::new();
	for account in accounts {
		let spendable = account_at(&head, account).await?.1;
		if spendable < amount {
			transfers.push(signer.transfer(account, amount - spendable)?);
		}
	}
	if transfers.is_empty() {
		return Ok(0);
	}
	let topped = transfers.len();
	let batches: Vec<Vec<u8>> = if transfers.len() > 1
		&& signer
			.metadata()
			.pallet_by_name("Utility")
			.and_then(|p| p.call_variant_by_name("batch_all"))
			.is_some()
	{
		let utility = signer.metadata().pallet_by_name("Utility").context("Utility")?;
		let (pallet_index, call_index) = (
			utility.call_index(),
			utility.call_variant_by_name("batch_all").context("batch_all")?.index,
		);
		transfers
			.chunks(100)
			.map(|chunk| {
				let mut call = vec![pallet_index, call_index];
				Compact(chunk.len() as u32).encode_to(&mut call);
				for c in chunk {
					call.extend(c);
				}
				call
			})
			.collect()
	} else {
		transfers
	};
	let mut nonces = HashMap::new();
	let mut pending = Vec::new();
	for call in &batches {
		pending.push(send(client, signer, funder, call, &mut nonces).await.context("funding")?);
	}
	eprintln!("submitted {} funding transactions for {topped} accounts", pending.len());
	while !pending.is_empty() {
		tokio::time::sleep(Duration::from_secs(1)).await;
		let mut done = resolve(client, &pending, timeout).await?;
		done.sort_by_key(|(i, _)| std::cmp::Reverse(*i));
		for (i, result) in done {
			result.context("funding transaction")?;
			pending.remove(i);
		}
	}
	let head = client.finalized_now().await?;
	for account in accounts {
		let spendable = account_at(&head, account).await?.1;
		ensure!(
			spendable >= amount,
			"funding failed: {} can spend {spendable}, less than {amount}",
			show(account)
		);
	}
	Ok(topped)
}

/// Removes the runner file when dropped.
struct Remove(PathBuf);
impl Drop for Remove {
	fn drop(&mut self) {
		let _ = std::fs::remove_file(&self.0);
	}
}

/// Runs the call file with node: its `setup` (whose transactions this signs, sends and waits
/// for), then `call` for each of `count` transactions. Returns the call data.
async fn run_script(
	client: &Client,
	script: &Path,
	target: &str,
	seed: &Key,
	keys: &[Key],
	count: u32,
	timeout: Duration,
) -> Result<Vec<Vec<u8>>> {
	let script = script
		.canonicalize()
		.with_context(|| format!("call file {}", script.display()))?;
	let dir = script.parent().context("call file has no directory")?;
	let runner = dir.join(format!(".polkameter-calls-{}.mjs", std::process::id()));
	std::fs::write(&runner, RUNNER)
		.with_context(|| format!("cannot write {}", runner.display()))?;
	let _remove = Remove(runner.clone());
	let mut child = tokio::process::Command::new("node")
		.arg(&runner)
		.arg(&script)
		.arg(target)
		.current_dir(dir)
		.stdin(std::process::Stdio::piped())
		.stdout(std::process::Stdio::piped())
		.kill_on_drop(true)
		.spawn()
		.context("cannot start node (calls mode needs Node.js 22.18 or later on PATH)")?;
	let mut stdin = child.stdin.take().context("runner stdin")?;
	let mut lines =
		tokio::io::BufReader::new(child.stdout.take().context("runner stdout")?).lines();
	let accounts: Vec<String> = keys.iter().map(|k| show(&k.account())).collect();
	let init = json!({"accounts": accounts, "seedAccount": show(&seed.account()), "count": count});
	stdin.write_all(format!("{init}\n").as_bytes()).await?;
	// Setup transactions are signed against the head when they are sent.
	let mut signer = Signer::new(client, Some(64)).await?;
	let mut nonces = HashMap::new();
	let mut pending: Vec<(u64, Pending)> = Vec::new();
	let mut tick = tokio::time::interval(Duration::from_secs(1));
	let mut calls = None;
	while calls.is_none() || !pending.is_empty() {
		tokio::select! {
			line = lines.next_line(), if calls.is_none() => {
				let line = line?.context("the call file's runner exited without returning calls; see the plugin log")?;
				let message: Value = serde_json::from_str(&line).with_context(|| format!("runner wrote {line:?}"))?;
				if let Some(error) = message["error"].as_str() {
					bail!("the call file failed: {error}");
				}
				if let Some(list) = message["calls"].as_array() {
					let decoded = list
						.iter()
						.map(|c| hex::decode(polkameter_plugin_sdk::strip_0x(c.as_str().unwrap_or_default())))
						.collect::<Result<Vec<_>, _>>()
						.context("calls must be hex call data")?;
					calls = Some(decoded);
					continue;
				}
				let id = message["id"].as_u64().context("runner message without id")?;
				let call = hex::decode(polkameter_plugin_sdk::strip_0x(message["submit"].as_str().unwrap_or_default()))
					.context("submit must be hex call data")?;
				let key = match message["from"].as_u64() {
					None => seed,
					Some(i) => keys.get(i as usize).with_context(|| format!("setup sends from account {i}, but there are {}", keys.len()))?,
				};
				if signer.block_number() + 32 < client.finalized_now().await?.block_number() {
					signer = Signer::new(client, Some(64)).await?;
				}
				match send(client, &signer, key, &call, &mut nonces).await {
					Ok(p) => pending.push((id, p)),
					Err(e) => stdin.write_all(format!("{}\n", json!({"id": id, "error": e.to_string()})).as_bytes()).await?,
				}
			},
			_ = tick.tick(), if !pending.is_empty() => {
				let list: Vec<Pending> = pending.iter().map(|(_, p)| Pending { hash: p.hash, signer: p.signer.clone(), nonce: p.nonce, from: p.from, sent: p.sent }).collect();
				let mut done = resolve(client, &list, timeout).await?;
				done.sort_by_key(|(i, _)| std::cmp::Reverse(*i));
				for (i, result) in done {
					let (id, p) = pending.remove(i);
					let reply = match result {
						Ok(()) => json!({"id": id, "ok": true, "hash": hex0x(p.hash)}),
						Err(e) => json!({"id": id, "error": e.to_string()}),
					};
					// The runner may have exited after it returned its calls.
					let _ = stdin.write_all(format!("{reply}\n").as_bytes()).await;
				}
			},
		}
	}
	drop(stdin);
	let _ = child.wait().await;
	let calls = calls.expect("loop ends with calls");
	ensure!(
		calls.len() == count as usize,
		"the call file returned {} calls, expected {count}",
		calls.len()
	);
	Ok(calls)
}

/// A JSON argument as a SCALE value, with `$i` and `$account` replaced: numbers and digit
/// strings are integers, `0x` strings bytes, arrays unnamed composites, an object with one
/// capitalised key a variant (`{"Id": "$account"}`), any other object a named composite.
pub fn json_value(json: &Value, i: u32, account: &[u8]) -> Result<ScaleValue> {
	Ok(match json {
		Value::Bool(b) => ScaleValue::bool(*b),
		Value::Number(n) => match (n.as_u64(), n.as_i64()) {
			(Some(u), _) => ScaleValue::u128(u.into()),
			(None, Some(s)) => ScaleValue::i128(s.into()),
			_ => bail!("argument {n} is not a whole number"),
		},
		Value::String(s) if s == "$i" => ScaleValue::u128(i.into()),
		Value::String(s) if s == "$account" => ScaleValue::from_bytes(account),
		Value::String(s) if s.starts_with("0x") => ScaleValue::from_bytes(
			hex::decode(&s[2..]).with_context(|| format!("argument {s} is not hex"))?,
		),
		Value::String(s) if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) => {
			ScaleValue::u128(s.parse().with_context(|| format!("argument {s}"))?)
		},
		Value::String(s) => ScaleValue::string(s.clone()),
		Value::Array(items) => ScaleValue::unnamed_composite(
			items.iter().map(|v| json_value(v, i, account)).collect::<Result<Vec<_>>>()?,
		),
		Value::Object(map)
			if map.len() == 1 && map.keys().all(|k| k.starts_with(char::is_uppercase)) =>
		{
			let (name, inner) = map.iter().next().expect("one key");
			let fields = match inner {
				Value::Null => Vec::new(),
				Value::Array(items) => {
					items.iter().map(|v| json_value(v, i, account)).collect::<Result<_>>()?
				},
				other => vec![json_value(other, i, account)?],
			};
			ScaleValue::unnamed_variant(name.clone(), fields)
		},
		Value::Object(map) => ScaleValue::named_composite(
			map.iter()
				.map(|(k, v)| Ok((k.clone(), json_value(v, i, account)?)))
				.collect::<Result<Vec<_>>>()?,
		),
		Value::Null => bail!("argument null is not supported"),
	})
}

/// `count` calls of `pallet.call(args)` without a call file. `args` is a JSON array of the
/// call's arguments in order, or an object of them by name.
fn simple_calls(
	signer: &Signer,
	name: &str,
	args: &Value,
	keys: &[Key],
	count: u32,
) -> Result<Vec<Vec<u8>>> {
	let (pallet, call) = name
		.split_once('.')
		.with_context(|| format!("input call must be Pallet.call, got {name:?}"))?;
	let variant = signer
		.metadata()
		.pallet_by_name(pallet)
		.and_then(|p| p.call_variant_by_name(call))
		.with_context(|| format!("the runtime has no call {name}"))?;
	let names: Vec<String> =
		variant.fields.iter().map(|f| f.name.clone().unwrap_or_default()).collect();
	(0..count)
		.map(|i| {
			let account = keys[i as usize % keys.len()].account();
			let fields = match args {
				Value::Array(items) => {
					items.iter().map(|v| json_value(v, i, &account)).collect::<Result<Vec<_>>>()?
				},
				Value::Object(map) => names
					.iter()
					.map(|n| {
						json_value(
							map.get(n).with_context(|| format!("input args has no {n}"))?,
							i,
							&account,
						)
					})
					.collect::<Result<Vec<_>>>()?,
				Value::Null => Vec::new(),
				other => bail!("input args must be a JSON array or object, got {other}"),
			};
			signer.call_data(pallet, call, fields)
		})
		.collect()
}

/// The `prepare-calls` operation.
#[allow(clippy::too_many_lines)]
pub async fn prepare_calls(inputs: &Value, context: &Context) -> Result<Value> {
	let target = inputs::target(inputs)?;
	let count = inputs::small(inputs, "count", 1..=inputs::MAX_COUNT, None)?;
	let probes = inputs::small(inputs, "probes", 1..=inputs::MAX_PROBES, None)?;
	let signers = inputs::Signers::parse(inputs)?;
	let mortality = inputs::mortality(inputs)?;
	let timeout =
		Duration::from_secs(inputs::integer(inputs, "timeout-seconds", 1..=3_600, Some(120))?);
	let client = crate::connect(&target).await?;
	let signer = Signer::new(&client, mortality).await?;
	let seed = seed_key(inputs::text(inputs, "seed")?, signer.format.eth)?;
	let total = signers.flood + signers.probe;
	let mut keys = derive(&seed, &signers.derivation, 0..total)?;
	let probe_keys = keys.split_off(signers.flood as usize);
	let accounts: Vec<Vec<u8>> = keys.iter().chain(&probe_keys).map(Key::account).collect();
	ensure!(
		!accounts.contains(&seed.account()),
		"the seed is one of the test accounts; use another derivation"
	);
	// Fund: the amount given, or a share of what the seed can spend, at most a million
	// existential deposits each.
	let amount = match &inputs["amount"] {
		Value::Null => {
			let spendable = account_at(&client.finalized_now().await?, &seed.account()).await?.1;
			let ed = client.existential_deposit().await.unwrap_or(1);
			(spendable / (2 * (u128::from(total) + 1))).min(ed.saturating_mul(1_000_000))
		},
		_ => inputs::amount(inputs, "amount", None)?,
	};
	let funded = fund(&client, &signer, &seed, &accounts, amount, timeout).await?;
	eprintln!("funded {funded} of {} test accounts with {amount}", accounts.len());
	let calls = match (&inputs["script"], &inputs["call"]) {
		(Value::String(script), Value::Null) => {
			run_script(&client, Path::new(script), &target, &seed, &keys, count, timeout).await?
		},
		(Value::Null, Value::String(name)) => {
			let args: Value = match &inputs["args"] {
				Value::String(s) => serde_json::from_str(s).context("input args must be JSON")?,
				other => other.clone(),
			};
			simple_calls(&signer, name, &args, &keys, count)?
		},
		_ => bail!("give exactly one of the inputs script (a call file) or call (Pallet.call)"),
	};
	// Signed against the finalized head after setup, at the nonces the pool expects now.
	let signer = Signer::new(&client, mortality).await?;
	if let (Some(period), Ok(expected)) = (
		mortality,
		inputs::integer(inputs, "expected-seconds", 1..=inputs::MAX_EXPECTED_SECONDS, None),
	) {
		let block_s = client.recent_block_seconds().await?.unwrap_or(6.0);
		let lasts = era_period(period) as f64 * block_s;
		ensure!(
			lasts >= expected as f64,
			"mortality {period} lasts about {lasts:.0} s at {block_s:.1} s blocks, less than the {expected} s the run needs; raise it"
		);
	}
	let mut base = Vec::new();
	for key in keys.iter().chain(&probe_keys) {
		base.push(next_nonce(&client, &key.account()).await?);
	}
	let n = keys.len();
	let started = std::time::Instant::now();
	let mut flood = Vec::with_capacity(calls.len());
	for (i, call) in calls.iter().enumerate() {
		let signer_index = i % n;
		let nonce = base[signer_index] + (i / n) as u64;
		let bytes = signer.sign(call, &keys[signer_index], nonce)?;
		let metadata = json!({"lane": "flood", "index": i, "signer": signer_index, "nonce": nonce});
		flood.push((bytes, metadata));
	}
	let mut probe_txs = Vec::with_capacity(probes as usize);
	for i in 0..probes as usize {
		let k = i % probe_keys.len();
		let nonce = base[n + k] + (i / probe_keys.len()) as u64;
		let remark = crate::payload(&context.run_id, "probe", i as u32, 32);
		let call = signer.call_data("System", "remark", vec![ScaleValue::from_bytes(remark)])?;
		let bytes = signer.sign(&call, &probe_keys[k], nonce)?;
		probe_txs
			.push((bytes, json!({"lane": "probe", "index": i, "signer": n + k, "nonce": nonce})));
	}
	eprintln!(
		"signed {} calls and {} probes in {:.1} s",
		flood.len(),
		probe_txs.len(),
		started.elapsed().as_secs_f64()
	);
	let mortal = mortality.is_some();
	let mut checks =
		vec![("the first call", &flood[0].0), ("the last call", &flood[flood.len() - 1].0)];
	checks.extend(probe_txs.iter().take(probe_keys.len()).map(|(b, _)| ("a probe", b)));
	for (what, bytes) in checks {
		client.validate(bytes, what).await.map_err(|e| {
			anyhow!("{what} is refused: {}{}", crate::describe(&e, mortal), defaulted_note(&signer))
		})?;
	}
	let prepared = |txs: Vec<(Vec<u8>, Value)>| -> Vec<PreparedTx> {
		txs.into_iter()
			.map(|(bytes, metadata)| PreparedTx::new(&bytes, metadata))
			.collect()
	};
	let (flood, probe_txs) = (prepared(flood), prepared(probe_txs));
	let dir = &context.artifact_dir;
	Ok(json!({
		"flood": Artifact::write(dir, "calls-flood.json", &flood)?,
		"probes": Artifact::write(dir, "calls-probes.json", &probe_txs)?,
		"identity": crate::identity(&client).await?,
		"summary": {
			"calls": flood.len(),
			"probes": probe_txs.len(),
			"accounts": n,
			"accountType": if signer.format.eth { "AccountId20 (ECDSA)" } else { "AccountId32 (sr25519)" },
			"funded": funded,
			"amount": amount.to_string(),
			"preparedAt": signer.block_number(),
			"validUntil": signer.valid_until(),
			"defaultedExtensions": signer.defaulted,
		},
	}))
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn ethereum_keys_sign_for_their_address() {
		let alith = seed_key("//Alith", true).unwrap();
		assert_eq!(hex0x(alith.account()), "0xf24ff3a9cf04c71dbc94d0b566f7a27b94566cac");
		let keys = derive(&alith, "//polkameter//", 0..3).unwrap();
		let accounts: HashSet<Vec<u8>> = keys.iter().map(Key::account).collect();
		assert_eq!(accounts.len(), 3);
		// The runtime recovers the signer from keccak-256 of the payload, as fp_account does.
		let Key::Eth(k) = &keys[1] else { panic!() };
		let signature = eth::Signature(keys[1].sign(b"payload").try_into().unwrap());
		assert!(eth::verify(&signature, b"payload", &k.public_key()));
		assert!(seed_key("//Alice", true).is_err());
		assert!(matches!(seed_key("//Alice", false).unwrap(), Key::Sr(_)));
	}

	#[test]
	fn json_arguments_become_values() {
		let v = json_value(&json!({"Id": "$account"}), 3, &[7; 32]).unwrap();
		assert_eq!(
			v.to_string(),
			ScaleValue::unnamed_variant("Id", [ScaleValue::from_bytes([7; 32])]).to_string()
		);
		assert_eq!(json_value(&json!("$i"), 3, &[]).unwrap(), ScaleValue::u128(3));
		assert_eq!(
			json_value(&json!("1000000000000000000000"), 0, &[]).unwrap(),
			ScaleValue::u128(10u128.pow(21))
		);
	}
}
