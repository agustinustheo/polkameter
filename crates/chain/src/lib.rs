//! A Polkadot SDK node over RPC: typed reads, events and submission.

pub mod client;
pub mod reads;
pub mod tx;
pub mod value;

pub use client::{AtBlock, ChainError, Client, Fault, LayoutChanged, Refusal, decode_err, hex0x};
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
/// Part of the plugin API (used by out-of-tree plugins).
pub use subxt::dynamic::Value;
/// Part of the plugin API (used by out-of-tree plugins).
pub use subxt_signer::sr25519::Keypair;
pub use tx::{ChainInfo, tx_hash};
