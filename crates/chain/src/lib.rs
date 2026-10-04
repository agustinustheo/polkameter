//! A Polkadot SDK node over RPC: typed reads, events, submission, and the check that a
//! transaction encoding still matches the running runtime's extension layout.

pub mod client;
pub mod reads;
pub mod tx;
pub mod value;

pub use client::{AtBlock, ChainError, Client, Fault, LayoutChanged};
pub use reads::{calls, entries, events, fetch, has_prefix, runtime_call};
#[doc(hidden)]
pub use scale_decode as scale_decode_reexport;
pub use scale_decode::DecodeAsType;
#[doc(hidden)]
pub use scale_value as scale_value_reexport;
pub use subxt::dynamic::Value;
pub use subxt_signer::sr25519::Keypair;
pub use tx::{ChainInfo, tx_hash};
