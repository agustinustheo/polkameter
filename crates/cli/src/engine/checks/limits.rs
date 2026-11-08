//! Limits the checks compare against. They are placeholders until the load target and the
//! budgets are agreed (technical-design non-fun-tests README, Open Questions); change them here only.

/// See each field.
#[derive(Debug)]
pub struct Limits {
	/// Slot-based authoring budget per block (polkadot-omni-node `aura.rs`, authoring_duration 2 s).
	pub authoring_deadline_s: f64,
	/// PVF backing timeout; approval allows 12 s.
	pub backing_timeout_s: f64,
	/// Relay slots of the observed parachain a step may miss beyond the idle rate.
	pub max_extra_missed_slots: u32,
	/// Our waiting txs above which a block that ends empty is a fault.
	pub pool_stuck_min_txs: f64,
	/// Share of the block interval pool maintenance may take (p95).
	pub max_maintain_share_of_block: f64,
	/// Txs waiting for pool validation at a step end.
	pub max_validation_backlog: f64,
}

/// The limits.
pub const LIMITS: Limits = Limits {
	authoring_deadline_s: 2.0,
	backing_timeout_s: 2.0,
	max_extra_missed_slots: 1,
	pool_stuck_min_txs: 10.0,
	max_maintain_share_of_block: 0.5,
	max_validation_backlog: 1_000.0,
};
