//! The checks, one file per outcome (outcomes.md). Each exports `CHECKS`, and every check
//! returns a `Verdict` that `run` turns into a `CheckResult`.

mod block_production;
mod pool;
mod pvf;
mod recorded;
mod recycler;

use crate::Check;

/// Every check, one list per outcome, in the order of the summary.
pub fn all() -> Vec<Check> {
	[block_production::CHECKS, pvf::CHECKS, pool::CHECKS, recycler::CHECKS, recorded::CHECKS]
		.concat()
}
