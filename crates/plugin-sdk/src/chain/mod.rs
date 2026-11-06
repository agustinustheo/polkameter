//! A Polkadot SDK node over RPC: typed reads, events and submission, plus signed transactions
//! and test signers for any runtime.
//!
//! [`Client`] connects to one node. [`Client::finalized`] pins a block, and [`fetch`],
//! [`entries`] and [`events`] read it. [`Client::call_data`] encodes a call from its pallet and
//! call names. [`Client::validate`] asks the node whether it would accept a transaction, and
//! [`Client::submit`] sends one.
//!
//! A load plugin uses these to prepare transactions before the load starts: read what it needs,
//! sign the calls with a [`TxBuilder`] from [`Client::tx_builder`] at explicit nonces from
//! [`Client::nonce`], and return them as [`PreparedTx`](crate::PreparedTx) values.
//! [`TxBuilder::sign_batch`] signs many calls for one signer with consecutive nonces, and
//! [`derive_range`] derives many test signers from one secret URI with [`keypair`].
//!
//! ```no_run
//! use polkameter_plugin_sdk::PreparedTx;
//! use polkameter_plugin_sdk::chain::{Client, Value, fetch};
//!
//! # async fn example(prepared: PreparedTx, signer: [u8; 32]) -> anyhow::Result<()> {
//! let client = Client::connect("ws://127.0.0.1:9944").await?;
//! let info = client.chain_info().await?;
//! println!("spec version {}, transaction version {}", info.spec_version, info.tx_version);
//!
//! // Read a plain storage value at the finalized block. Keys are `()` for a plain value.
//! let at = client.finalized().await?;
//! let number: Option<u32> = fetch(&at, "System", "Number", ()).await?;
//! println!("finalized block {number:?}");
//!
//! // Encode a call, and read the signer's next nonce.
//! let remark = client.call_data("System", "remark", vec![Value::from_bytes(b"hello")]).await?;
//! let nonce = client.nonce(signer).await?;
//! println!("{} bytes of call data, nonce {nonce}", remark.len());
//!
//! // Check a signed transaction against the best block, then send it.
//! let bytes = prepared.decode()?;
//! client.validate(&bytes, "prepared transaction").await?;
//! let hash = client.submit(&bytes).await?;
//! println!("submitted {hash}");
//! # Ok(())
//! # }
//! ```
//!
//! In a load plan, PolkaMeter submits the prepared transactions itself. Use [`Client::submit`]
//! for setup transactions that must land before the load.

#![warn(missing_docs, clippy::needless_pass_by_value, clippy::too_many_lines)]

pub mod client;
pub mod keys;
pub mod reads;
pub mod sign;
pub mod tx;
pub mod value;

pub use client::{AtBlock, ChainError, Client, Fault, LayoutChanged, Refusal, decode_err, hex0x};
pub use keys::{
	KeyError, account_id, derive, derive_range, junctions, keypair, parse_account, ss58,
};
pub use reads::{
	Event, calls, entries, events, failed_extrinsics, fetch, has_prefix, runtime_call,
};
#[doc(hidden)]
/// Part of the plugin API (used by out-of-tree plugins).
pub use scale_decode as scale_decode_reexport;
pub use scale_decode::DecodeAsType;
#[doc(hidden)]
/// Part of the plugin API (used by out-of-tree plugins).
pub use scale_value as scale_value_reexport;
pub use sign::{AccountInfo, Call, Mortality, SignError, TxBuilder, TxParams, account_at};
/// A dynamic SCALE value: the fields of a call for [`Client::call_data`], or a storage key.
/// Re-exported from subxt, so a plugin does not need its own subxt dependency.
pub use subxt::dynamic::Value;
/// An sr25519 keypair to sign transactions with. Re-exported from subxt-signer.
pub use subxt_signer::sr25519::Keypair;
pub use tx::{ChainInfo, tx_hash};
