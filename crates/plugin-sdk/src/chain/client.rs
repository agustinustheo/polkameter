//! One node (a parachain or the relay) over RPC: subxt for typed reads at a block, plain JSON-RPC for the
//! calls subxt doesn't wrap (`state_call`, `author_*`).
//!
//! Errors here are the chain's, not ours: the caller decides whether one stops a setup or is a
//! result of the run.

use subxt::rpcs::{RpcClient, rpc_params};
use subxt::{
	OnlineClient, PolkadotConfig,
	dynamic::{self, Value},
};

use crate::chain::tx::ChainInfo;

/// subxt at a block.
pub type AtBlock = subxt::client::ClientAtBlock<
	PolkadotConfig,
	subxt::client::OnlineClientAtBlockImpl<PolkadotConfig>,
>;

/// The chain did not answer, or answered something we can't read.
#[derive(Debug, thiserror::Error)]
pub enum ChainError {
	/// An RPC call failed.
	#[error("{method}: {source}")]
	Rpc {
		/// Method.
		method: &'static str,
		/// Why.
		source: subxt::rpcs::Error,
	},
	/// A subxt read failed.
	#[error("{what}: {detail}")]
	Read {
		/// What we read.
		what: &'static str,
		/// Why.
		detail: String,
	},
	/// An answer did not decode as our types: our view of the runtime is wrong.
	#[error("{what} does not decode: {detail}")]
	Decode {
		/// What we decoded.
		what: &'static str,
		/// Why.
		detail: String,
	},
	/// `validate_transaction` refused a tx.
	#[error("{what} is not valid: {kind:?} {code} (raw {raw})")]
	Invalid {
		/// The tx.
		what: String,
		/// Invalid or Unknown.
		kind: Refusal,
		/// Custom or variant code.
		code: u8,
		/// The first bytes of the answer.
		raw: String,
	},
	/// Waited too long.
	#[error("{0}")]
	Timeout(String),
}

/// Why `validate_transaction` refused a tx: the runtime's `TransactionValidityError`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
	/// The tx can never be valid.
	Invalid,
	/// The chain can't tell whether the tx is valid.
	Unknown,
}

/// Whose fault an error is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
	/// The chain or its RPC: a result during a run, a reason to stop during setup.
	Chain,
	/// Our code or our view of the runtime (decode, encode, metadata): stops the run.
	Tool,
}

impl ChainError {
	/// Whose fault it is. A read that fails to decode means our types no longer match the
	/// runtime, so it is ours; an RPC error or a timeout is the chain's.
	pub fn fault(&self) -> Fault {
		match self {
			ChainError::Rpc { .. }
			| ChainError::Read { .. }
			| ChainError::Timeout(_)
			| ChainError::Invalid { .. } => Fault::Chain,
			ChainError::Decode { .. } => Fault::Tool,
		}
	}
}

pub(crate) fn read_any<E: std::fmt::Display>(what: &'static str) -> impl FnOnce(E) -> ChainError {
	move |e| ChainError::Read { what, detail: e.to_string() }
}

/// An answer that did not decode as our types: our view of the runtime is wrong.
pub fn decode_err<E: std::fmt::Display>(what: &'static str) -> impl FnOnce(E) -> ChainError {
	move |e| ChainError::Decode { what, detail: e.to_string() }
}

/// Our encoding no longer matches the runtime: an error of our tools, so the run stops.
#[derive(Debug, thiserror::Error)]
#[error("the runtime's tx extensions changed; ours: {}, live: {}", ours.join(", "), live.join(", "))]
pub struct LayoutChanged {
	ours: Vec<&'static str>,
	live: Vec<String>,
}

/// A connected node.
#[derive(Debug, Clone)]
pub struct Client {
	api: OnlineClient<PolkadotConfig>,
	rpc: RpcClient,
}

impl Client {
	/// Connects to `url`.
	pub async fn connect(url: &str) -> Result<Self, ChainError> {
		let rpc = RpcClient::from_insecure_url(url)
			.await
			.map_err(|source| ChainError::Rpc { method: "connect", source })?;
		let api = OnlineClient::from_rpc_client(rpc.clone()).await.map_err(read_any("metadata"))?;
		Ok(Self { api, rpc })
	}

	/// A raw JSON-RPC call.
	pub async fn request<T: serde::de::DeserializeOwned>(
		&self,
		method: &'static str,
		params: subxt::rpcs::client::RpcParams,
	) -> Result<T, ChainError> {
		self.rpc
			.request(method, params)
			.await
			.map_err(|source| ChainError::Rpc { method, source })
	}

	/// subxt at the finalized block.
	pub async fn finalized(&self) -> Result<AtBlock, ChainError> {
		self.api.at_current_block().await.map_err(read_any("finalized block"))
	}

	/// subxt at `hash`.
	pub async fn at(&self, hash: [u8; 32]) -> Result<AtBlock, ChainError> {
		self.api.at_block(subxt::utils::H256(hash)).await.map_err(read_any("block"))
	}

	/// The subxt client, for streams.
	pub fn api(&self) -> &OnlineClient<PolkadotConfig> {
		&self.api
	}

	/// Spec, tx version and genesis, read once at startup.
	pub async fn chain_info(&self) -> Result<ChainInfo, ChainError> {
		let version: serde_json::Value =
			self.request("state_getRuntimeVersion", rpc_params![]).await?;
		let genesis: String = self.request("chain_getBlockHash", rpc_params![0]).await?;
		let field = |k: &str| {
			version[k].as_u64().and_then(|v| u32::try_from(v).ok()).ok_or_else(|| {
				ChainError::Read { what: "runtime version", detail: format!("no {k}") }
			})
		};
		Ok(ChainInfo {
			spec_version: field("specVersion")?,
			tx_version: field("transactionVersion")?,
			genesis: hex32(&genesis)?,
		})
	}

	/// Checks the live extension list against our tx layout.
	pub async fn check_extensions(
		&self,
		expected: &[&'static str],
	) -> Result<Result<(), LayoutChanged>, ChainError> {
		let at = self.finalized().await?;
		let live: Vec<String> = at
			.metadata_ref()
			.extrinsic()
			.transaction_extensions_to_use_for_encoding()
			.map(|e| e.identifier().to_owned())
			.collect();
		let ours: Vec<&'static str> = expected.to_vec();
		Ok(if live == ours { Ok(()) } else { Err(LayoutChanged { ours, live }) })
	}

	/// The hash of block `number` on the node's canonical chain; `None` when the node doesn't
	/// have the block (a fork keeps none from before its bite).
	async fn try_block_hash(&self, number: u32) -> Result<Option<[u8; 32]>, ChainError> {
		let hash: Option<String> = self.request("chain_getBlockHash", rpc_params![number]).await?;
		hash.map(|h| hex32(&h)).transpose()
	}

	/// The hash of block `number`; an error when the node doesn't have it.
	pub async fn block_hash(&self, number: u32) -> Result<[u8; 32], ChainError> {
		self.try_block_hash(number).await?.ok_or_else(|| ChainError::Read {
			what: "block hash",
			detail: format!("the node has no block {number}"),
		})
	}

	/// The best block number now.
	pub async fn best_number(&self) -> Result<u32, ChainError> {
		let best: serde_json::Value = self.request("chain_getHeader", rpc_params![]).await?;
		u32::from_str_radix(best["number"].as_str().unwrap_or("0x0").trim_start_matches("0x"), 16)
			.map_err(decode_err("header number"))
	}

	/// Encodes a call by pallet and call name, with metadata.
	pub async fn call_data(
		&self,
		pallet: &str,
		call: &str,
		fields: Vec<Value>,
	) -> Result<Vec<u8>, ChainError> {
		let at = self.finalized().await?;
		at.transactions()
			.call_data(&dynamic::tx(pallet, call, fields))
			.map_err(decode_err("call data"))
	}

	/// `Sudo.sudo(inner)`.
	pub async fn sudo(&self, inner: &[u8]) -> Result<Vec<u8>, ChainError> {
		let at = self.finalized().await?;
		let md = at.metadata_ref();
		let call = subxt::ext::scale_value::scale::decode_as_type(
			&mut &inner[..],
			md.outer_enums().call_enum_ty(),
			md.types(),
		)
		.map_err(decode_err("inner call"))?;
		at.transactions()
			.call_data(&dynamic::tx("Sudo", "sudo", vec![call.remove_context()]))
			.map_err(decode_err("sudo call"))
	}

	/// The next nonce of `account`.
	pub async fn nonce(&self, account: [u8; 32]) -> Result<u32, ChainError> {
		let ss58 = subxt::utils::AccountId32(account).to_string();
		self.request("system_accountNextIndex", rpc_params![ss58]).await
	}

	/// `TaggedTransactionQueue_validate_transaction` at the best block.
	pub async fn validate(&self, bytes: &[u8], what: &str) -> Result<(), ChainError> {
		let best: String = self.request("chain_getBlockHash", rpc_params![]).await?;
		let mut arg = vec![2u8]; // TransactionSource::External
		arg.extend_from_slice(bytes);
		arg.extend(hex32(&best)?);
		let out: String = self
			.request(
				"state_call",
				rpc_params!["TaggedTransactionQueue_validate_transaction", hex0x(arg), best],
			)
			.await?;
		let out = hex::decode(out.trim_start_matches("0x")).map_err(read_any("validate answer"))?;
		if out.first() == Some(&0) {
			return Ok(());
		}
		let kind = if out.get(1) == Some(&0) { Refusal::Invalid } else { Refusal::Unknown };
		let code = out.get(3).or(out.get(2)).copied().unwrap_or(0);
		Err(ChainError::Invalid {
			what: what.to_owned(),
			kind,
			code,
			raw: hex::encode(&out[..out.len().min(4)]),
		})
	}

	/// `author_submitExtrinsic`; the node's hash.
	pub async fn submit(&self, bytes: &[u8]) -> Result<String, ChainError> {
		self.request("author_submitExtrinsic", rpc_params![hex0x(bytes)]).await
	}

	/// The raw extrinsics of a block.
	pub async fn body(&self, hash: [u8; 32]) -> Result<Vec<Vec<u8>>, ChainError> {
		let block: serde_json::Value =
			self.request("chain_getBlock", rpc_params![hex0x(hash)]).await?;
		let list = block["block"]["extrinsics"].as_array().ok_or_else(|| ChainError::Read {
			what: "block body",
			detail: "no extrinsics".into(),
		})?;
		list.iter()
			.map(|x| {
				hex::decode(x.as_str().unwrap_or("").trim_start_matches("0x"))
					.map_err(read_any("extrinsic hex"))
			})
			.collect()
	}
}

/// `0x` and the hex of `bytes`: how the RPC and our files write a hash.
pub fn hex0x(bytes: impl AsRef<[u8]>) -> String {
	format!("0x{}", hex::encode(bytes))
}

fn hex32(s: &str) -> Result<[u8; 32], ChainError> {
	hex::decode(s.trim_start_matches("0x"))
		.ok()
		.and_then(|b| b.try_into().ok())
		.ok_or_else(|| ChainError::Read { what: "hash", detail: s.to_owned() })
}
