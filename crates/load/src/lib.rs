//! The runner every scenario shares: baseline, load until the chain fails (or the plan ends),
//! recovery, loss check. A scenario brings lanes of txs and a plan; everything else is here.
//!
//! The tracker ([`tracker`]) owns every tx and is plain code: it is fed by the send tick, the
//! submit replies ([`sender`]) and the block follower ([`follower`]), so it has no lock and is
//! tested without a network.

pub mod follower;
pub mod loss;
pub mod plan;
pub mod recovery;
pub mod rules;
pub mod runner;
pub mod scenario;
pub mod sender;
pub mod source;
pub mod steps;
pub mod submit;
pub mod tracker;

pub use plan::{Lane, Plan, StepPlan};
pub use polkameter_files::{Millis, now_ms};
pub use scenario::{BoxFuture, StateCheck};
pub use source::{QueueSource, Tx, TxHash, probe_count};
