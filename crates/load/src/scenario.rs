//! What a scenario brings: its options, its setup, its lanes and an optional state check.

use std::future::Future;
use std::pin::Pin;

use polkameter_chain::{ChainError, ChainInfo, Client};
use polkameter_files::summary::StateSample;

use crate::plan::{Lane, Plan, Ramp};
use crate::source::TxHash;

/// The chain is not ready for the scenario (no rings, a tx that doesn't validate), or our own
/// code failed while setting it up: the run stops before any load, with no result.
#[derive(Debug, thiserror::Error)]
pub enum SetupError {
	/// The chain answered, but not as the setup needs.
	#[error("{what}: {source}")]
	Chain {
		/// The setup step.
		what: &'static str,
		/// Why.
		source: ChainError,
	},
	/// Anything else in the scenario's own code.
	#[error("{what}: {detail}")]
	Scenario {
		/// The setup step.
		what: &'static str,
		/// Why.
		detail: String,
	},
}

/// What `prepare` gets.
#[derive(Debug)]
pub struct Setup {
	/// People.
	pub client: Client,
	/// Spec, tx version, genesis.
	pub chain: ChainInfo,
	/// Random per run; people and keys come from it.
	pub run_seed: [u8; 32],
	/// People's block interval before the run.
	pub block_interval_s: f64,
	/// Probes the runner will ask each lane for (baseline + recovery).
	pub probes: usize,
}

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
	/// What the scenario knows of a tx, for lost.jsonl (e.g. the member and slot of a claim).
	fn describe(&self, _tx: &TxHash) -> serde_json::Value {
		serde_json::Value::Null
	}
}

/// What `prepare` returns.
pub struct Prepared {
	/// The loads, in plan rate order.
	pub lanes: Vec<Lane>,
	/// What the run can spend, for the summary, e.g. "750 people × 20 slots".
	pub budget: String,
	/// Scenario numbers for summary.json.
	pub extra: serde_json::Value,
	/// The state check, if the scenario has one.
	pub state_check: Option<Box<dyn StateCheck>>,
	/// Keys the recycler recorder follows to a built root (voucher scenarios): the Members
	/// collection and the keys.
	pub vouchers: Option<([u8; 32], Vec<[u8; 32]>)>,
}

impl std::fmt::Debug for Prepared {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("Prepared")
			.field("lanes", &self.lanes)
			.field("budget", &self.budget)
			.finish_non_exhaustive()
	}
}

/// One non-functional test.
pub trait Scenario {
	/// Run id prefix and subcommand, e.g. "stmt-flood".
	const ID: &'static str;
	/// Title in the summary.
	const TITLE: &'static str;
	/// Runtime call or other isolated component under stress.
	const ARTIFACT: &'static str;
	/// What one submitted operation does.
	const ARTIFACT_DESCRIPTION: &'static str;
	/// Why the operation matters to the product flow.
	const ARTIFACT_CONTEXT: &'static str;
	/// The scenario's own options (clap `Args` in the binary).
	type Options: serde::Serialize + Send + Sync;
	/// Ramp defaults; the command line can change each one.
	const RAMP: Ramp;
	/// The steps, from the options and the ramp; pure, so it is checked before any setup.
	fn plan(_opts: &Self::Options, r: &Ramp) -> Plan {
		match r.growth {
			Some(growth) => Plan::geometric(r.start, growth, r.interval_s, r.steps),
			None => Plan::ramp(r.start, r.step, r.interval_s, r.steps),
		}
	}
	/// Sets the chain up and builds the lanes.
	fn prepare(
		opts: &Self::Options,
		setup: &Setup,
	) -> impl Future<Output = Result<Prepared, SetupError>> + Send;
}
