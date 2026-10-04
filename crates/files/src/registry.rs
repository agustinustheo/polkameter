//! Every metric a run reads or writes, with its type, unit and labels.
//!
//! Node metrics are read from `/metrics`; the preflight checks each `# TYPE` against this list,
//! because names don't tell the type (`substrate_sub_txpool_validations_scheduled` is a counter
//! without `_total`). Our own metrics start with `polkameter_`; each is a typed handle, so a gauge
//! can't be written as a counter and a label can't be missing (the label count is in the type).

use std::marker::PhantomData;

/// Prometheus type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
	/// Goes up; read by the difference between two times.
	Counter,
	/// Read as it is.
	Gauge,
	/// Read by the difference of its buckets.
	Histogram,
}

/// The outcomes in technical-design `test-design/outcomes.md`, plus `Run` for the ramp itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
	/// Why blocks end, build time.
	#[serde(rename = "block production")]
	BlockProduction,
	/// Relay slots, PVF time, disputes.
	Pvf,
	/// Drain, loss, refusals, pool work.
	Pool,
	/// Fork depth and settling.
	Forks,
	/// Declared weight against measured time.
	Weights,
	/// The run itself.
	Run,
}

impl Outcome {
	/// The name in summary.json and summary.md.
	pub fn name(self) -> &'static str {
		match self {
			Outcome::BlockProduction => "block production",
			Outcome::Pvf => "pvf",
			Outcome::Pool => "pool",
			Outcome::Forks => "forks",
			Outcome::Weights => "weights",
			Outcome::Run => "run",
		}
	}
}

/// One metric definition.
#[derive(Debug, Clone, Copy)]
pub struct Def {
	/// Family name (a counter's samples end in `_total`).
	pub name: &'static str,
	/// Type.
	pub kind: Kind,
	/// Help text for `run.om`.
	pub help: &'static str,
	/// The labels the evaluator reads.
	pub labels: &'static [&'static str],
	/// Upper bounds of our histograms; node histograms bring theirs.
	pub buckets: &'static [f64],
	/// The node jobs that serve it (node metrics only).
	pub from: &'static [&'static str],
}

/// Marker types for [`Metric`].
pub mod kind {
	/// A counter handle.
	#[derive(Debug)]
	pub struct Counter;
	/// A gauge handle.
	#[derive(Debug)]
	pub struct Gauge;
	/// A histogram handle.
	#[derive(Debug)]
	pub struct Histogram;
}

/// A typed handle to one of our metrics: `K` is the kind, `N` the number of labels.
#[derive(Debug)]
pub struct Metric<K, const N: usize> {
	/// The definition.
	pub def: Def,
	_kind: PhantomData<K>,
}

impl<K, const N: usize> Metric<K, N> {
	/// A handle for a metric defined outside this registry, e.g. by a plugin. `N` must equal
	/// `def.labels.len()`.
	pub const fn new(def: Def) -> Self {
		assert!(def.labels.len() == N, "label count does not match the definition");
		Self { def, _kind: PhantomData }
	}
}

macro_rules! polkameter_metrics {
    ($( $id:ident: $k:ident [$($label:literal),*] $(buckets $b:expr,)? $name:literal, $help:literal; )*) => {
        $(
            #[doc = $help]
            pub const $id: Metric<kind::$k, { <[&str]>::len(&[$($label),*]) }> = Metric {
                def: Def { name: $name, kind: Kind::$k, help: $help, labels: &[$($label),*], buckets: polkameter_metrics!(@b $($b)?), from: &[] },
                _kind: PhantomData,
            };
        )*
        /// Every `polkameter_*` metric.
        pub const STRESS_METRICS: &[Def] = &[$($id.def),*];
    };
    (@b) => { &[] };
    (@b $b:expr) => { $b };
}

polkameter_metrics! {
	STEP: Gauge [] "polkameter_step", "The ramp step running now; -1 outside the ramp.";
	PHASE: Gauge ["phase"] "polkameter_phase", "1 for the phase running now, 0 for the others: baseline, ramp, recovery, done.";
	TX_SENT: Counter ["call"] "polkameter_tx_sent_total", "Txs sent.";
	TX_REJECTED: Counter ["call", "reason"] "polkameter_tx_rejected_total", "Txs the node refused at submit, by error.";
	TX_INCLUDED: Counter ["call"] "polkameter_tx_included_total", "Our txs seen in a best block.";
	TX_FAILED: Counter ["call"] "polkameter_tx_failed_total", "Our included txs with an ExtrinsicFailed event.";
	TX_EXPIRED: Counter ["call"] "polkameter_tx_expired_total", "Our txs not in a block after their mortality ended.";
	TX_INCLUSION: Histogram ["call"] buckets &[0.5, 1.0, 2.0, 3.0, 4.0, 6.0, 8.0, 12.0, 18.0, 24.0, 36.0, 60.0, 120.0],
		"polkameter_tx_inclusion_seconds", "Time from send to the first best block with the tx.";
	SCRAPE_FAILED: Counter ["job", "instance"] "polkameter_scrape_failed_total", "Scrapes of a node's /metrics that failed (no answer, timeout, HTTP error).";
	RELAY_FINALIZED_BLOCKS: Counter [] "polkameter_relay_finalized_blocks_total", "Finalized relay blocks the chain recorder read.";
	PARA_SLOTS: Counter ["para"] "polkameter_para_slots_total", "Relay slots offered to the para: at each finalized relay block, the cores whose claim queue starts with the para.";
	PARA_BACKED: Counter ["para"] "polkameter_para_backed_total", "Candidates backed on the relay (ParaInclusion.CandidateBacked).";
	PARA_INCLUDED: Counter ["para"] "polkameter_para_included_total", "Candidates included on the relay (ParaInclusion.CandidateIncluded).";
	PARA_TIMED_OUT: Counter ["para"] "polkameter_para_timed_out_total", "Candidates backed but not available in time (ParaInclusion.CandidateTimedOut).";
	RELAY_DISPUTES: Counter [] "polkameter_relay_dispute_total", "Disputes started on the relay (ParasDisputes.DisputeInitiated), for any parachain; the event names only the candidate.";
}

const COLLATOR: &[&str] = &["collator"];
const RELAY_IN_COLLATOR: &[&str] = &["collator-relay"];
const VALIDATOR: &[&str] = &["validator"];

const fn node(
	name: &'static str,
	kind: Kind,
	labels: &'static [&'static str],
	from: &'static [&'static str],
	help: &'static str,
) -> Def {
	Def { name, kind, help, labels, buckets: &[], from }
}

/// Every node metric we read, checked against the node's `# TYPE` in the preflight.
pub const NODE_METRICS: &[Def] = &[
	node(
		"substrate_proposer_end_proposal_reason",
		Kind::Counter,
		&["reason"],
		COLLATOR,
		"Why a block stopped taking txs: no_more_transactions, hit_deadline, hit_block_size_limit, hit_block_weight_limit, transactions_forbidden.",
	),
	node(
		"substrate_proposer_block_constructed",
		Kind::Histogram,
		&[],
		COLLATOR,
		"Time to build a block.",
	),
	node(
		"polkadot_pvf_execution_time",
		Kind::Histogram,
		&[],
		VALIDATOR,
		"PVF execution time, for all parachains together and without a backing/approval split. Buckets go up to 12 s.",
	),
	node(
		"polkadot_pvf_execution_queued_time",
		Kind::Histogram,
		&[],
		VALIDATOR,
		"Time a PVF execution job waits before a worker takes it, for all parachains together. It grows under load and adds to the time until backing.",
	),
	node(
		"polkadot_parachain_candidate_validation_pov_size",
		Kind::Histogram,
		&["compressed"],
		VALIDATOR,
		"PoV size per validated candidate, for all parachains together. Buckets stop at 8 MiB (16 KiB times 2^9), below the 10 MiB limit; the exact size is the parachain's System.BlockWeight.proof_size.",
	),
	node(
		"polkadot_parachain_candidate_backing_candidates_seconded_total",
		Kind::Counter,
		&[],
		VALIDATOR,
		"Candidates this validator seconded, for all parachains together.",
	),
	node(
		"polkadot_parachain_provisioner_backable_vs_in_block",
		Kind::Histogram,
		&[],
		VALIDATOR,
		"Backable candidates the relay block author left out of its block (backable minus backed in the block).",
	),
	node(
		"polkadot_parachain_collations_generated_total",
		Kind::Counter,
		&[],
		RELAY_IN_COLLATOR,
		"Collations the collator generated.",
	),
	node(
		"polkadot_parachain_collation_advertisements_made_total",
		Kind::Counter,
		&[],
		RELAY_IN_COLLATOR,
		"Collation advertisements sent to validators.",
	),
	node(
		"polkadot_parachain_collations_sent_requested_total",
		Kind::Counter,
		&[],
		RELAY_IN_COLLATOR,
		"Collations validators asked for.",
	),
	node(
		"polkadot_parachain_collations_sent_total",
		Kind::Counter,
		&[],
		RELAY_IN_COLLATOR,
		"Collations sent to validators (PoV delivered).",
	),
	node(
		"polkadot_parachain_collation_backing_latency",
		Kind::Histogram,
		&[],
		RELAY_IN_COLLATOR,
		"Relay blocks from a collation's relay parent until it is backed. Its count is the collations backed.",
	),
	node(
		"polkadot_parachain_collation_inclusion_latency",
		Kind::Histogram,
		&[],
		RELAY_IN_COLLATOR,
		"Relay blocks from backed to included; 1 is normal, more means availability is slow. Its count is the collations included.",
	),
	node(
		"polkadot_parachain_collation_expired",
		Kind::Histogram,
		&["state"],
		RELAY_IN_COLLATOR,
		"Collations that expired, by the last state they reached: advertised (delivery), fetched (validation or backing), backed (availability).",
	),
	node(
		"polkadot_parachain_validation_requests_total",
		Kind::Counter,
		&["validity"],
		VALIDATOR,
		"Candidate validations, valid or invalid.",
	),
	node(
		"polkadot_parachain_candidate_disputes_total",
		Kind::Counter,
		&[],
		VALIDATOR,
		"Disputes raised.",
	),
	node(
		"substrate_sub_txpool_unwatched_txs",
		Kind::Gauge,
		&[],
		COLLATOR,
		"Unwatched txs in the pool (ours are sent with author_submitExtrinsic, so they count here).",
	),
	node(
		"substrate_ready_transactions_number",
		Kind::Gauge,
		&[],
		COLLATOR,
		"Txs in the ready queue.",
	),
	node(
		"substrate_sub_txpool_validations_scheduled",
		Kind::Counter,
		&[],
		COLLATOR,
		"Txs scheduled for validation.",
	),
	node(
		"substrate_sub_txpool_validations_finished",
		Kind::Counter,
		&[],
		COLLATOR,
		"Txs that finished validation.",
	),
	node(
		"substrate_sub_txpool_timing_event_dropped",
		Kind::Histogram,
		&[],
		COLLATOR,
		"Time from submit to the Dropped event. Its count is the number of dropped txs.",
	),
	node(
		"substrate_sub_txpool_timing_event_invalid",
		Kind::Histogram,
		&[],
		COLLATOR,
		"Time from submit to the Invalid event. Its count is the number of invalid txs.",
	),
	node(
		"substrate_sub_txpool_maintain_duration_seconds",
		Kind::Histogram,
		&[],
		COLLATOR,
		"Pool maintenance time per block.",
	),
	node(
		"substrate_number_leaves",
		Kind::Gauge,
		&[],
		COLLATOR,
		"Known chain leaves; more than 1 means there is a fork.",
	),
	node(
		"substrate_block_height",
		Kind::Gauge,
		&["status"],
		COLLATOR,
		"Best, finalized and sync target block number. Best minus finalized is the finality lag.",
	),
	node(
		"substrate_sub_txpool_timing_event_retracted",
		Kind::Histogram,
		&[],
		COLLATOR,
		"Time from submit to the Retracted event (the tx's block left the best chain).",
	),
	node(
		"substrate_sub_txpool_resubmitted_retracted_txs_total",
		Kind::Counter,
		&[],
		COLLATOR,
		"Txs put back into the pool from retracted blocks.",
	),
];

/// The definition of a node or stress family.
pub fn def(family: &str) -> Option<&'static Def> {
	NODE_METRICS.iter().chain(STRESS_METRICS).find(|d| d.name == family)
}

/// The node family a sample name belongs to (`x_bucket` -> `x` for a histogram), if we read it.
pub fn node_family_of(sample: &str) -> Option<&'static Def> {
	if let Some(d) = NODE_METRICS.iter().find(|d| d.name == sample) {
		return Some(d);
	}
	let base = ["_bucket", "_sum", "_count"].iter().find_map(|s| sample.strip_suffix(s))?;
	NODE_METRICS.iter().find(|d| d.name == base && d.kind == Kind::Histogram)
}
