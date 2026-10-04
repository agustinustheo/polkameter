//! The optional check of chain state after a run: did the included transactions have the effect
//! they should.

use std::future::Future;
use std::pin::Pin;

use polkameter_chain::{ChainError, Client};
use polkameter_files::summary::StateSample;

use crate::source::TxHash;

/// A boxed future, for the object-safe [`StateCheck`].
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Checks chain state at the finalized block `at` for a sample of included txs.
pub trait StateCheck: Send + Sync {
	/// Reads state for `included` at `at`.
	fn check<'a>(
		&'a self,
		client: &'a Client,
		included: &'a [TxHash],
		at: [u8; 32],
	) -> BoxFuture<'a, Result<StateSample, ChainError>>;
	/// What the workload knows of a tx, for lost.jsonl.
	fn describe(&self, _tx: &TxHash) -> serde_json::Value {
		serde_json::Value::Null
	}
}
