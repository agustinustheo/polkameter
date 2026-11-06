//! Signed transactions for any runtime, built from its metadata: an explicit nonce, mortality and
//! tip, and many transactions for one signer with consecutive nonces.
//!
//! A [`TxBuilder`] holds the metadata and runtime identity of one block. Signing with it needs no
//! RPC, so a plugin can prepare thousands of transactions before a load starts. The builder uses
//! subxt's dynamic API, so it works for any runtime whose transaction extensions subxt knows
//! (the FRAME `Check*` extensions, `ChargeTransactionPayment`, `ChargeAssetTxPayment`,
//! `CheckMetadataHash`, and extensions without explicit data, such as `CheckWeight`). A runtime
//! with another extension that carries data fails with [`SignError`] rather than producing an
//! invalid transaction.

use subxt::{
	PolkadotConfig,
	config::PolkadotExtrinsicParamsBuilder,
	dynamic::{self, Value},
	utils::{AccountId32, H256, MultiSignature},
};
use subxt_signer::sr25519::Keypair;

use crate::chain::{
	client::{AtBlock, ChainError, Client, decode_err},
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
	/// rounds `period` up to a power of two between 4 and 65536.
	Mortal {
		/// Blocks the transaction stays valid for.
		period: u64,
		/// The block the period starts at.
		from_number: u64,
		/// That block's hash.
		from_hash: [u8; 32],
	},
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

	/// A mortality of `period` blocks starting at the builder's block.
	pub fn mortal(&self, period: u64) -> Mortality {
		Mortality::Mortal {
			period,
			from_number: self.at.block_number(),
			from_hash: self.at.block_hash().0,
		}
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
	fn sign_errors_name_the_call() {
		let error = SignError::new("System.remark transaction", "no such call");
		assert_eq!(error.to_string(), "System.remark transaction: no such call");
	}
}
