//! One node (a parachain or the relay) over RPC: subxt for typed reads at a block, plain JSON-RPC for the
//! calls subxt doesn't wrap (`state_call`, `author_*`).
//!
//! Errors here are the chain's, not ours: the caller decides whether one stops a setup or is a
//! result of the run.

use subxt::dynamic::{self, Value};
use subxt::{OnlineClient, PolkadotConfig};
use subxt_rpcs::{RpcClient, rpc_params};

use crate::reads::fetch;
use crate::tx::{ChainInfo, tx_hash};

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
		source: subxt_rpcs::Error,
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
	#[error("{what} is not valid: {kind} {code} (raw {raw})")]
	Invalid {
		/// The tx.
		what: String,
		/// Invalid or Unknown.
		kind: &'static str,
		/// Custom or variant code.
		code: u8,
		/// The first bytes of the answer.
		raw: String,
	},
	/// Waited too long.
	#[error("{0}")]
	Timeout(String),
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

/// Our encoding no longer matches the runtime: an error of our tools, so the run stops.
#[derive(Debug, thiserror::Error)]
#[error("the runtime's tx extensions changed; ours: {ours:?}, live: {live:?}")]
pub struct LayoutChanged {
	ours: Vec<&'static str>,
	live: Vec<String>,
}

pub(crate) fn read_any<E: std::fmt::Display>(what: &'static str) -> impl FnOnce(E) -> ChainError {
	move |e| ChainError::Read { what, detail: e.to_string() }
}

pub(crate) fn decode_err<E: std::fmt::Display>(what: &'static str) -> impl FnOnce(E) -> ChainError {
	move |e| ChainError::Decode { what, detail: e.to_string() }
}

/// A connected node.
#[derive(Debug, Clone)]
pub struct Client {
	api: OnlineClient<PolkadotConfig>,
	rpc: RpcClient,
	/// Its WebSocket URL.
	pub url: String,
}

impl Client {
	/// Connects to `url`.
	pub async fn connect(url: &str) -> Result<Self, ChainError> {
		let rpc = RpcClient::from_insecure_url(url)
			.await
			.map_err(|source| ChainError::Rpc { method: "connect", source })?;
		let api = OnlineClient::from_rpc_client(rpc.clone()).await.map_err(read_any("metadata"))?;
		Ok(Self { api, rpc, url: url.to_owned() })
	}

	/// A raw JSON-RPC call.
	pub async fn request<T: serde::de::DeserializeOwned>(
		&self,
		method: &'static str,
		params: subxt_rpcs::client::RpcParams,
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

	/// The running runtime's normal class limit (ref time, proof size): `System.BlockWeights`.
	/// Read it here, never from the runtime source: live previewnet allows 1.5 s, `main` 1.7 s.
	pub async fn normal_limit(&self) -> Result<(u64, u64), ChainError> {
		#[derive(scale_decode::DecodeAsType)]
		struct W {
			ref_time: u64,
			proof_size: u64,
		}
		#[derive(scale_decode::DecodeAsType)]
		struct Class {
			max_total: Option<W>,
		}
		#[derive(scale_decode::DecodeAsType)]
		struct PerClass {
			normal: Class,
		}
		#[derive(scale_decode::DecodeAsType)]
		struct BlockWeights {
			per_class: PerClass,
		}
		let at = self.finalized().await?;
		let w = at
			.constants()
			.entry(dynamic::constant::<BlockWeights>("System", "BlockWeights"))
			.map_err(decode_err("System.BlockWeights"))?;
		Ok(w.per_class.normal.max_total.map_or((0, 0), |m| (m.ref_time, m.proof_size)))
	}

	/// The hash of block `number` on the node's canonical chain; `None` when the node doesn't
	/// have the block (a fork keeps none from before its bite).
	pub async fn try_block_hash(&self, number: u32) -> Result<Option<[u8; 32]>, ChainError> {
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

	/// The parent hash of block `hash`.
	pub async fn parent(&self, hash: [u8; 32]) -> Result<[u8; 32], ChainError> {
		let header: serde_json::Value = self
			.request("chain_getHeader", rpc_params![format!("0x{}", hex::encode(hash))])
			.await?;
		hex32(header["parentHash"].as_str().ok_or_else(|| ChainError::Read {
			what: "header",
			detail: format!("no parent hash for 0x{}", hex::encode(hash)),
		})?)
	}

	/// The best block number now.
	pub async fn best_number(&self) -> Result<u32, ChainError> {
		let best: serde_json::Value = self.request("chain_getHeader", rpc_params![]).await?;
		u32::from_str_radix(best["number"].as_str().unwrap_or("0x0").trim_start_matches("0x"), 16)
			.map_err(decode_err("header number"))
	}

	/// The chain's block interval: the `Timestamp.Now` span of the last 60 blocks since the fork
	/// started, over their count. Not a median or trimmed mean of gaps: on a fork the timestamps
	/// come in bunches (0 s / 12 s pairs on 1 core, 12 s then five 0 s on 3 cores), and only the
	/// span is right. A fork just started has fewer blocks: this waits for 20 of them.
	pub async fn block_interval_s(&self) -> Result<f64, ChainError> {
		let deadline = tokio::time::Instant::now() + INTERVAL_WAIT;
		loop {
			let number = self.best_number().await?;
			let all = self.timestamps(number).await?;
			let stamps = since_restart(&all);
			if let Some(s) = interval_s(stamps) {
				return Ok(s);
			}
			if tokio::time::Instant::now() >= deadline {
				return Err(ChainError::Timeout(format!(
					"block interval: {} blocks since the fork started at block {number}, {MIN_INTERVALS} needed, after {INTERVAL_WAIT:?}",
					stamps.len().saturating_sub(1)
				)));
			}
			tokio::time::sleep(std::time::Duration::from_secs(6)).await;
		}
	}

	/// `Timestamp.Now` of block `number` and up to 60 before it, newest first; the walk stops
	/// at a block the node doesn't have (a fork keeps none from before its bite).
	async fn timestamps(&self, number: u32) -> Result<Vec<u64>, ChainError> {
		let mut stamps = Vec::new();
		for n in (number.saturating_sub(60)..=number).rev() {
			let Some(hash) = self.try_block_hash(n).await? else { break };
			stamps.push(
				fetch::<(), u64>(&self.at(hash).await?, "Timestamp", "Now", ())
					.await?
					.unwrap_or(0),
			);
		}
		Ok(stamps)
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
		let call = scale_value::scale::decode_as_type(
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
				rpc_params![
					"TaggedTransactionQueue_validate_transaction",
					format!("0x{}", hex::encode(arg)),
					best
				],
			)
			.await?;
		let out = hex::decode(out.trim_start_matches("0x")).map_err(read_any("validate answer"))?;
		if out.first() == Some(&0) {
			return Ok(());
		}
		let kind = if out.get(1) == Some(&0) { "Invalid" } else { "Unknown" };
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
		self.request("author_submitExtrinsic", rpc_params![format!("0x{}", hex::encode(bytes))])
			.await
	}

	/// The raw extrinsics of a block.
	pub async fn body(&self, hash: [u8; 32]) -> Result<Vec<Vec<u8>>, ChainError> {
		let block: serde_json::Value = self
			.request("chain_getBlock", rpc_params![format!("0x{}", hex::encode(hash))])
			.await?;
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

	/// Hashes of the ready txs in the node's pool.
	pub async fn pending(&self) -> Result<Vec<[u8; 32]>, ChainError> {
		let list: Vec<String> = self.request("author_pendingExtrinsics", rpc_params![]).await?;
		list.iter()
			.map(|x| {
				hex::decode(x.trim_start_matches("0x"))
					.map(|b| tx_hash(&b))
					.map_err(read_any("pending hex"))
			})
			.collect()
	}
}

fn hex32(s: &str) -> Result<[u8; 32], ChainError> {
	hex::decode(s.trim_start_matches("0x"))
		.ok()
		.and_then(|b| b.try_into().ok())
		.ok_or_else(|| ChainError::Read { what: "hash", detail: s.to_owned() })
}

/// A gap between two blocks longer than any slot: the bite to the fork's first block (or a
/// stall). On a fork the longest normal gap is 12 s.
const RESTART_GAP_MS: u64 = 60_000;
/// Blocks the interval spans at least: enough whole timestamp bunches (six blocks on 3 cores)
/// for the span to be right.
const MIN_INTERVALS: usize = 20;
/// How long to wait for `MIN_INTERVALS` blocks after a fork started.
const INTERVAL_WAIT: std::time::Duration = std::time::Duration::from_secs(120);

/// The newest blocks of `stamps` (newest first, ms) up to the first gap over `RESTART_GAP_MS`:
/// the blocks since the fork started.
fn since_restart(stamps: &[u64]) -> &[u64] {
	let first_gap = stamps.windows(2).position(|w| w[0].saturating_sub(w[1]) > RESTART_GAP_MS);
	&stamps[..first_gap.map_or(stamps.len(), |i| i + 1)]
}

/// The block interval, s, over `stamps` (newest first, ms); `None` with fewer than
/// `MIN_INTERVALS` blocks or a zero span.
fn interval_s(stamps: &[u64]) -> Option<f64> {
	let (newest, oldest) = (stamps.first()?, stamps.last()?);
	let intervals = stamps.len() - 1;
	let span = newest.saturating_sub(*oldest);
	(intervals >= MIN_INTERVALS && span > 0).then(|| span as f64 / intervals as f64 / 1000.0)
}

#[cfg(test)]
mod tests {
	use super::*;

	/// Newest first: `n` blocks of 3-core bunches (12 s, then five at 0 s) ending at `end` ms.
	fn bunched(end: u64, n: u64) -> Vec<u64> {
		(0..n).map(|k| end - k.div_ceil(6) * 12_000).collect()
	}

	#[test]
	fn the_interval_is_the_span_over_whole_bunches() {
		assert_eq!(interval_s(&bunched(1_000_000, 61)), Some(2.0));
	}

	#[test]
	fn blocks_from_before_the_fork_are_left_out() {
		// 31 fork blocks at 2 s, then the bite's gap of 5 minutes, then live blocks at 12 s.
		let mut stamps = bunched(1_000_000, 31);
		let bite = stamps[30] - 300_000;
		stamps.extend((0..30).map(|k| bite - k * 12_000));
		assert_eq!(since_restart(&stamps).len(), 31);
		assert_eq!(interval_s(since_restart(&stamps)), Some(2.0));
		// Before the fix: the span over all 61 blocks.
		assert!(interval_s(&stamps).unwrap() > 9.0);
	}

	#[test]
	fn too_few_blocks_since_the_fork_give_no_interval() {
		assert_eq!(interval_s(&bunched(1_000_000, 20)), None);
		assert_eq!(interval_s(&[5_000; 30]), None);
		assert_eq!(interval_s(&[]), None);
	}
}
