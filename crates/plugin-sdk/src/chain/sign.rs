//! Signed transactions for any runtime, built from its metadata: an explicit nonce, mortality and
//! tip, and many transactions for one signer with consecutive nonces.
//!
//! A [`TxBuilder`] holds the metadata and runtime identity of one block. Signing with it needs no
//! RPC, so a plugin can prepare thousands of transactions before a load starts. The builder uses
//! subxt's dynamic API, so it works for any runtime whose transaction extensions subxt knows
//! (the FRAME `Check*` extensions, `ChargeTransactionPayment`, `ChargeAssetTxPayment`,
//! `CheckMetadataHash`, and extensions without explicit data, such as `CheckWeight`). A runtime
//! with another extension that carries data fails with [`SignError`] rather than producing an
//! invalid transaction. It signs with sr25519 keys, so the runtime's address type must be
//! `MultiAddress` or `AccountId32` and its signature type `MultiSignature`;
//! [`TxBuilder::check_signing_types`] tells.
//!
//! A signed transaction commits to the runtime's spec and transaction versions. After a runtime
//! upgrade, the chain refuses transactions signed before it with a bad-signature error.

use subxt::{
	PolkadotConfig,
	config::PolkadotExtrinsicParamsBuilder,
	dynamic::{self, Value},
	utils::{AccountId32, H256, MultiSignature},
};
use subxt_signer::sr25519::Keypair;

use crate::chain::{
	client::{AtBlock, ChainError, Client, decode_err},
	reads::{fetch, runtime_call},
	value::{as_u128, field},
};

/// One call by pallet and call name, with its fields as dynamic values in declaration order.
#[derive(Debug, Clone)]
pub struct Call {
	/// The pallet, e.g. `Balances`.
	pub pallet: String,
	/// The call, e.g. `transfer_keep_alive`.
	pub name: String,
	/// The call's arguments, in order.
	pub fields: Vec<Value>,
}

impl Call {
	/// `pallet.name(fields)`.
	pub fn new(pallet: impl Into<String>, name: impl Into<String>, fields: Vec<Value>) -> Self {
		Self { pallet: pallet.into(), name: name.into(), fields }
	}
}

/// How long a transaction stays valid.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mortality {
	/// Valid at any block.
	Immortal,
	/// Valid for `period` blocks from block `from_number`, whose hash is `from_hash`. The chain
	/// rounds `period` up to a power of two between 4 and 65536. For periods above 4096, the
	/// era starts at a multiple of `period / 4096`, so `from_number` must be such a block:
	/// [`TxBuilder::mortal`] picks it, and [`era_birth`] computes it.
	Mortal {
		/// Blocks the transaction stays valid for.
		period: u64,
		/// The block the period starts at.
		from_number: u64,
		/// That block's hash.
		from_hash: [u8; 32],
	},
}

/// The period the chain uses for a requested mortality of `period` blocks: the next power of two,
/// from 4 to 65536.
pub fn era_period(period: u64) -> u64 {
	period.checked_next_power_of_two().unwrap_or(1 << 16).clamp(4, 1 << 16)
}

/// The first block of the era of a transaction signed at block `number` to stay valid for
/// `period` blocks. The era encodes its start in 12 bits, so for periods above 4096 the start is
/// rounded down to a multiple of `period / 4096`: block 49 with a period of 65536 gives block 48.
/// The transaction must be signed against that block's hash.
pub fn era_birth(period: u64, number: u64) -> u64 {
	let quantum = (era_period(period) >> 12).max(1);
	number - number % quantum
}

/// Why the runtime's address or signature types are not ones an sr25519 key can sign for, from
/// the types' metadata; `Ok` for `MultiAddress` (with an `AccountId32` `Id`) or `AccountId32`,
/// and `MultiSignature` (or another signature enum with an `Sr25519` variant).
pub fn check_signing_types(
	types: &scale_info::PortableRegistry,
	address: u32,
	signature: u32,
) -> Result<(), String> {
	use scale_info::TypeDef;
	let name = |id: u32| {
		types.resolve(id).map_or_else(
			|| format!("type {id}"),
			|ty| match ty.path.segments.last() {
				Some(last) => last.clone(),
				None => format!("{:?}", ty.type_def).chars().take(60).collect(),
			},
		)
	};
	let last = |id: u32| types.resolve(id).and_then(|ty| ty.path.segments.last().cloned());
	let variant_field = |id: u32, variant: &str| {
		types.resolve(id).and_then(|ty| match &ty.type_def {
			TypeDef::Variant(v) => v
				.variants
				.iter()
				.find(|x| x.name == variant)
				.map(|x| x.fields.first().map(|f| f.ty.id)),
			_ => None,
		})
	};
	let address_ok = match last(address).as_deref() {
		Some("AccountId32") => true,
		Some("MultiAddress") => variant_field(address, "Id")
			.flatten()
			.is_some_and(|inner| last(inner).as_deref() == Some("AccountId32")),
		_ => false,
	};
	let signature_ok = variant_field(signature, "Sr25519").is_some();
	if address_ok && signature_ok {
		return Ok(());
	}
	let inner = variant_field(address, "Id").flatten().map(|id| format!(" of {}", name(id)));
	Err(format!(
		"its address type is {}{} and its signature type {}; sr25519 keys need MultiAddress or AccountId32 addresses and MultiSignature (an AccountId20 or ECDSA-only runtime needs its own signer)",
		name(address),
		inner.unwrap_or_default(),
		name(signature)
	))
}

/// The values a signer chooses for one transaction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TxParams {
	/// The signer's nonce for this transaction.
	pub nonce: u64,
	/// How long it stays valid.
	pub mortality: Mortality,
	/// The tip, in the chain's smallest unit.
	pub tip: u128,
}

impl TxParams {
	/// An immortal transaction without a tip.
	pub fn nonce(nonce: u64) -> Self {
		Self { nonce, mortality: Mortality::Immortal, tip: 0 }
	}
}

/// A call or transaction that the runtime's metadata cannot encode, or a transaction extension
/// that the builder does not support.
#[derive(Debug, thiserror::Error)]
#[error("{what}: {detail}")]
pub struct SignError {
	what: String,
	detail: String,
}

impl SignError {
	fn new(what: impl Into<String>, detail: impl std::fmt::Display) -> Self {
		Self { what: what.into(), detail: detail.to_string() }
	}
}

/// An account's nonce and balances, from `System.Account`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct AccountInfo {
	/// The next nonce the chain expects, without transactions waiting in the pool.
	pub nonce: u64,
	/// Free balance.
	pub free: u128,
	/// Reserved balance.
	pub reserved: u128,
	/// Frozen balance, which the account cannot spend.
	pub frozen: u128,
}

impl AccountInfo {
	/// What the account can transfer or pay fees with: free minus frozen.
	pub fn spendable(&self) -> u128 {
		self.free.saturating_sub(self.frozen)
	}
}

#[derive(scale_decode::DecodeAsType)]
struct RawAccount {
	nonce: u64,
	// Read by name: older runtimes have `misc_frozen` and `fee_frozen` instead of `frozen`.
	data: Value,
}

/// Signs transactions against the metadata of one block, without RPC.
#[derive(Clone)]
pub struct TxBuilder {
	at: AtBlock,
}

impl std::fmt::Debug for TxBuilder {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("TxBuilder")
			.field("block", &self.at.block_number())
			.field("spec_version", &self.at.spec_version())
			.finish()
	}
}

impl TxBuilder {
	/// A builder for the runtime at `at`.
	pub fn new(at: AtBlock) -> Self {
		Self { at }
	}

	/// The block whose metadata the builder uses.
	pub fn at(&self) -> &AtBlock {
		&self.at
	}

	/// The number of the builder's block.
	pub fn block_number(&self) -> u64 {
		self.at.block_number()
	}

	/// A mortality of `period` blocks from the builder's block. For periods above 4096, the era
	/// starts up to `period / 4096 - 1` blocks earlier (see [`era_birth`]); this reads that
	/// block's hash from `System.BlockHash`, as the runtime does.
	pub async fn mortal(&self, period: u64) -> Result<Mortality, ChainError> {
		let number = self.at.block_number();
		let birth = era_birth(period, number);
		let from_hash = if birth == number {
			self.at.block_hash().0
		} else {
			let block = u32::try_from(birth).map_err(|_| ChainError::Read {
				what: "System.BlockHash",
				detail: format!("block {birth} does not fit in 32 bits"),
			})?;
			let hash: Option<H256> = fetch(&self.at, "System", "BlockHash", (block,)).await?;
			match hash {
				Some(hash) if hash != H256::zero() => hash.0,
				_ => {
					return Err(ChainError::Read {
						what: "System.BlockHash",
						detail: format!("no hash for block {birth}, where the era starts"),
					});
				},
			}
		};
		Ok(Mortality::Mortal { period, from_number: birth, from_hash })
	}

	/// Fails with the reason when the runtime's address or signature types are not ones an
	/// sr25519 key can sign for, such as `AccountId20` with ECDSA signatures.
	pub fn check_signing_types(&self) -> Result<(), SignError> {
		use subxt::ext::frame_decode::extrinsics::ExtrinsicTypeInfo as _;
		let metadata = self.at.metadata_ref();
		let info = metadata
			.extrinsic_signature_info()
			.map_err(|e| SignError::new("the runtime's transaction format", e))?;
		check_signing_types(metadata.types(), info.address_id, info.signature_id)
			.map_err(|detail| SignError::new("the runtime's transaction format", detail))
	}

	/// The name of the error in a `DispatchError::Module` value, such as
	/// `Balances.InsufficientBalance`, or the value as text for any other dispatch error.
	pub fn dispatch_error(&self, error: &Value) -> String {
		use crate::chain::value::{as_u64, nth, variant_name};
		let module = (variant_name(error) == Some("Module"))
			.then(|| nth(error, 0).or(Some(error)))
			.flatten()
			.and_then(|m| {
				let index = as_u64(field(m, "index")?)?;
				let code = as_u64(nth(field(m, "error")?, 0)?)?;
				let pallet =
					self.at.metadata_ref().pallet_by_call_index(u8::try_from(index).ok()?)?;
				let variant = pallet.error_variant_by_index(u8::try_from(code).ok()?)?;
				Some(format!("{}.{}", pallet.name(), variant.name))
			});
		module.unwrap_or_else(|| {
			let text = error.to_string();
			if text.len() > 200 { format!("{}...", &text[..200]) } else { text }
		})
	}

	/// True when the runtime has the call `pallet.call`.
	pub fn has_call(&self, pallet: &str, call: &str) -> bool {
		self.at
			.metadata_ref()
			.pallet_by_name(pallet)
			.and_then(|p| p.call_variant_by_name(call))
			.is_some()
	}

	/// True when the runtime has the constant `pallet.name`.
	pub fn has_constant(&self, pallet: &str, name: &str) -> bool {
		self.at
			.metadata_ref()
			.pallet_by_name(pallet)
			.and_then(|p| p.constant_by_name(name))
			.is_some()
	}

	/// The SCALE call data of `call`, checked against the metadata.
	pub fn call_data(&self, call: &Call) -> Result<Vec<u8>, SignError> {
		self.at
			.transactions()
			.call_data(&dynamic::tx(&call.pallet, &call.name, call.fields.clone()))
			.map_err(|e| SignError::new(format!("{}.{} call", call.pallet, call.name), e))
	}

	/// `call`, signed by `signer` with `params`: the bytes to submit.
	pub fn sign(
		&self,
		call: &Call,
		signer: &Keypair,
		params: &TxParams,
	) -> Result<Vec<u8>, SignError> {
		let what = || format!("{}.{} transaction", call.pallet, call.name);
		let payload = dynamic::tx(&call.pallet, &call.name, call.fields.clone());
		let builder = PolkadotExtrinsicParamsBuilder::<PolkadotConfig>::new()
			.nonce(params.nonce)
			.tip(params.tip);
		let builder = match params.mortality {
			Mortality::Immortal => builder.immortal(),
			Mortality::Mortal { period, from_number, from_hash } => {
				let birth = era_birth(period, from_number);
				if birth != from_number {
					return Err(SignError::new(
						what(),
						format!(
							"a {}-block era cannot start at block {from_number}; it starts at block {birth}, so sign against that block's hash (TxBuilder::mortal does)",
							era_period(period)
						),
					));
				}
				builder.mortal_from_unchecked(period, from_number, H256(from_hash))
			},
		};
		let mut signable = self
			.at
			.transactions()
			.create_signable_offline(&payload, builder.build())
			.map_err(|e| SignError::new(what(), e))?;
		let message = signable.signer_payload().map_err(|e| SignError::new(what(), e))?;
		let signature = MultiSignature::Sr25519(signer.sign(&message).0);
		let tx = signable
			.sign_with_account_and_signature(&AccountId32(signer.public_key().0), &signature)
			.map_err(|e| SignError::new(what(), e))?;
		Ok(tx.into_encoded())
	}

	/// `calls`, each signed by `signer`, with consecutive nonces from `first_nonce`. Every
	/// transaction gets the same mortality and tip.
	pub fn sign_batch<'a>(
		&self,
		signer: &Keypair,
		first_nonce: u64,
		calls: impl IntoIterator<Item = &'a Call>,
		mortality: Mortality,
		tip: u128,
	) -> Result<Vec<Vec<u8>>, SignError> {
		calls
			.into_iter()
			.enumerate()
			.map(|(i, call)| {
				let nonce = first_nonce
					.checked_add(i as u64)
					.ok_or_else(|| SignError::new("nonce", "overflows u64"))?;
				self.sign(call, signer, &TxParams { nonce, mortality, tip })
			})
			.collect()
	}
}

impl Client {
	/// The hash of the node's finalized head, from `chain_getFinalizedHead`. Unlike
	/// [`Client::finalized`], it asks the node on every call, so a loop that waits for
	/// finality sees each new block.
	pub async fn finalized_head(&self) -> Result<[u8; 32], ChainError> {
		let hash: String =
			self.request("chain_getFinalizedHead", subxt_rpcs::rpc_params![]).await?;
		hex::decode(hash.trim_start_matches("0x"))
			.ok()
			.and_then(|bytes| bytes.try_into().ok())
			.ok_or_else(|| ChainError::Read { what: "finalized head", detail: hash })
	}

	/// subxt at the node's finalized head, as [`Client::finalized_head`] reports it.
	pub async fn finalized_now(&self) -> Result<AtBlock, ChainError> {
		self.at(self.finalized_head().await?).await
	}

	/// A [`TxBuilder`] at the node's finalized head.
	pub async fn tx_builder(&self) -> Result<TxBuilder, ChainError> {
		Ok(TxBuilder::new(self.finalized_now().await?))
	}

	/// `System.Account` of `account` at the node's finalized head: its nonce and balances. An
	/// account that does not exist has a nonce of 0 and no balance.
	pub async fn account(&self, account: [u8; 32]) -> Result<AccountInfo, ChainError> {
		account_at(&self.finalized_now().await?, account).await
	}

	/// The fee the chain would charge for the signed transaction `bytes` now, from
	/// `TransactionPaymentApi.query_info` at the node's finalized head. Fails on a runtime
	/// without that API.
	pub async fn query_fee(&self, bytes: &[u8]) -> Result<u128, ChainError> {
		let at = self.finalized_now().await?;
		let output = at
			.metadata_ref()
			.runtime_api_trait_by_name("TransactionPaymentApi")
			.and_then(|api| api.method_by_name("query_info"))
			.map(|method| method.output_ty())
			.ok_or_else(|| ChainError::Read {
				what: "TransactionPaymentApi.query_info",
				detail: "the runtime has no TransactionPaymentApi".into(),
			})?;
		let len = u32::try_from(bytes.len()).map_err(decode_err("transaction length"))?;
		let mut args = bytes.to_vec();
		args.extend(len.to_le_bytes());
		let raw = runtime_call(&at, "TransactionPaymentApi_query_info", &args).await?;
		let info =
			scale_value::scale::decode_as_type(&mut &raw[..], output, at.metadata_ref().types())
				.map_err(decode_err("TransactionPaymentApi.query_info"))?
				.remove_context();
		field(&info, "partial_fee").and_then(as_u128).ok_or_else(|| ChainError::Decode {
			what: "TransactionPaymentApi.query_info",
			detail: "no partial_fee".into(),
		})
	}

	/// The spendable balance of `account` at the node's finalized head: free minus frozen.
	pub async fn spendable(&self, account: [u8; 32]) -> Result<u128, ChainError> {
		Ok(self.account(account).await?.spendable())
	}

	/// `Balances.ExistentialDeposit`: the smallest balance an account can hold.
	pub async fn existential_deposit(&self) -> Result<u128, ChainError> {
		let at = self.finalized().await?;
		at.constants()
			.entry(dynamic::constant::<u128>("Balances", "ExistentialDeposit"))
			.map_err(decode_err("Balances.ExistentialDeposit"))
	}
}

/// `System.Account` of `account` at the block `at`.
pub async fn account_at(at: &AtBlock, account: [u8; 32]) -> Result<AccountInfo, ChainError> {
	let raw: Option<RawAccount> =
		crate::chain::reads::fetch(at, "System", "Account", (account,)).await?;
	let Some(raw) = raw else { return Ok(AccountInfo::default()) };
	let balance = |name: &str| field(&raw.data, name).and_then(as_u128);
	Ok(AccountInfo {
		nonce: raw.nonce,
		free: balance("free").ok_or_else(|| ChainError::Decode {
			what: "System.Account",
			detail: "no free balance".into(),
		})?,
		reserved: balance("reserved").unwrap_or(0),
		frozen: balance("frozen")
			.or_else(|| balance("misc_frozen").max(balance("fee_frozen")))
			.unwrap_or(0),
	})
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn params_default_to_immortal_without_tip() {
		let params = TxParams::nonce(7);
		assert_eq!(params.nonce, 7);
		assert_eq!(params.mortality, Mortality::Immortal);
		assert_eq!(params.tip, 0);
	}

	#[test]
	fn large_periods_start_at_a_quantized_block() {
		// 65536 blocks: the era's start is a multiple of 16, so block 49 signs against block 48.
		assert_eq!(era_birth(65_536, 49), 48);
		assert_eq!(era_birth(65_536, 48), 48);
		assert_eq!(era_birth(8_192, 49), 48);
		assert_eq!(era_birth(4_096, 49), 49);
		assert_eq!(era_birth(1_024, 49), 49);
		assert_eq!(era_birth(100, 49), 49);
		assert_eq!(era_period(100), 128);
		assert_eq!(era_period(1 << 20), 65_536);
		assert_eq!(era_period(1), 4);
		// subxt's encoding agrees: the era it builds from block 48 has phase 48, which block 49
		// also rounds to.
		use subxt::ext::codec::Encode as _;
		let from_birth = subxt::utils::Era::mortal(65_536, 48).encode();
		assert_eq!(from_birth, subxt::utils::Era::mortal(65_536, 49).encode());
		assert_ne!(from_birth, subxt::utils::Era::mortal(65_536, 64).encode());
	}

	#[test]
	fn signing_types_accept_sr25519_accounts_only() {
		use scale_info::{
			Field, Path, PortableRegistryBuilder, Type, TypeDef, TypeDefComposite, TypeDefVariant,
			Variant, form::PortableForm,
		};
		let mut registry = PortableRegistryBuilder::new();
		let named = |segments: &[&str], def: TypeDef<PortableForm>| {
			Type::new(
				Path::from_segments_unchecked(segments.iter().map(|s| s.to_string())),
				vec![],
				def,
				vec![],
			)
		};
		let composite = || TypeDef::Composite(TypeDefComposite::new(vec![]));
		let variant = |name: &str, field: Option<u32>| {
			Variant::new(
				name.into(),
				field
					.map(|ty| vec![Field::new(None, ty.into(), None, vec![])])
					.unwrap_or_default(),
				0,
				vec![],
			)
		};
		let enumeration = |variants| TypeDef::Variant(TypeDefVariant::new(variants));
		let id32 =
			registry.register_type(named(&["sp_core", "crypto", "AccountId32"], composite()));
		let id20 = registry.register_type(named(&["fp_account", "AccountId20"], composite()));
		let multi = registry.register_type(named(
			&["sp_runtime", "multiaddress", "MultiAddress"],
			enumeration(vec![variant("Id", Some(id32)), variant("Index", None)]),
		));
		let multi_sig = registry.register_type(named(
			&["sp_runtime", "MultiSignature"],
			enumeration(vec![variant("Ed25519", None), variant("Sr25519", None)]),
		));
		let eth_sig =
			registry.register_type(named(&["fp_account", "EthereumSignature"], composite()));
		let types = registry.finish();
		assert_eq!(check_signing_types(&types, multi, multi_sig), Ok(()));
		assert_eq!(check_signing_types(&types, id32, multi_sig), Ok(()));
		let error = check_signing_types(&types, id20, eth_sig).unwrap_err();
		assert!(
			error.starts_with(
				"its address type is AccountId20 and its signature type EthereumSignature"
			),
			"{error}"
		);
		assert!(check_signing_types(&types, multi, eth_sig).is_err());
	}

	#[test]
	fn spendable_is_free_minus_frozen() {
		let info = AccountInfo { nonce: 0, free: 100, reserved: 7, frozen: 30 };
		assert_eq!(info.spendable(), 70);
		assert_eq!(AccountInfo { frozen: 200, ..info }.spendable(), 0);
	}

	#[test]
	fn sign_errors_name_the_call() {
		let error = SignError::new("System.remark transaction", "no such call");
		assert_eq!(error.to_string(), "System.remark transaction: no such call");
	}
}
