//! `RunData`: window math over a run's recorded series.
//!
//! Values at a time: node series are scraped, so a step edge takes the first scrape at or after
//! the edge (the edge scrape lands a few ms after it). Our own series only change on events, so
//! they take the last value at or before the time, or 0. A labelled counter shows up on its
//! first increment, so a node series that first appears after the node's first scrape is 0
//! before that.

use std::{collections::HashMap, fmt, ops::AddAssign};

use polkameter_files::{Series, Store, num, summary::Summary};
use serde::{Serialize, Serializer};

/// Label filter: every pair must match.
pub type Filter<'a> = &'a [(&'a str, &'a str)];

/// A phase of the run, as the `polkameter_phase` gauge names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
	/// Before the load.
	Baseline,
	/// The load's ramp steps.
	Ramp,
	/// After the load stopped.
	Recovery,
	/// The end of the run.
	Done,
}

impl Phase {
	const ALL: [Phase; 4] = [Phase::Baseline, Phase::Ramp, Phase::Recovery, Phase::Done];

	/// The name the gauge writes: `baseline`, `ramp`, `recovery` or `done`.
	pub fn name(self) -> &'static str {
		match self {
			Phase::Baseline => "baseline",
			Phase::Ramp => "ramp",
			Phase::Recovery => "recovery",
			Phase::Done => "done",
		}
	}

	fn from_name(name: &str) -> Option<Phase> {
		Self::ALL.into_iter().find(|p| p.name() == name)
	}
}

impl AsRef<str> for Phase {
	fn as_ref(&self) -> &str {
		self.name()
	}
}

/// What a window is, for check details and summary.json: "step 3", "recovery", "run".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Label {
	/// One ramp step.
	Step(u32),
	/// One phase of the run.
	Phase(Phase),
	/// The whole run.
	Run,
	/// A window a plugin names for its own check, e.g. "read".
	Named(&'static str),
}

impl From<&'static str> for Label {
	/// Part of the plugin API (used by out-of-tree plugins).
	fn from(name: &'static str) -> Self {
		Label::Named(name)
	}
}

impl fmt::Display for Label {
	fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self {
			Label::Step(step) => write!(f, "step {step}"),
			Label::Phase(phase) => f.write_str(phase.name()),
			Label::Run => f.write_str("run"),
			Label::Named(name) => f.write_str(name),
		}
	}
}

impl Serialize for Label {
	fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
		s.collect_str(self)
	}
}

/// A time window of the run, in ms.
#[derive(Debug, Clone, PartialEq)]
pub struct Window {
	/// Start.
	pub start: f64,
	/// End.
	pub end: f64,
	/// What the window is.
	pub label: Label,
}

/// A counter went down in a window: a node restarted, so that window has no result.
#[derive(Debug, Clone, thiserror::Error)]
#[error("{0}")]
pub struct CounterReset(pub String);

/// How late after an edge its scrape may land, in ms (scrapes are 5 s apart).
const EDGE_SCRAPE_MS: f64 = 1000.0;

/// The collator's labels: each block is counted once over the collators.
pub const COLLATOR: (&str, &str) = ("job", "collator");
/// The collator's counter of why a block stopped taking txs, per `reason`.
pub const END_REASON: &str = "substrate_proposer_end_proposal_reason";

fn node(s: &Series) -> (String, String) {
	let get = |k: &str| s.labels.get(k).cloned().unwrap_or_default();
	(get("job"), get("instance"))
}

fn value_at(s: &Series, t: f64, node_first: Option<f64>) -> f64 {
	let scraped = s.labels.get("job").is_none_or(|j| j != "stress");
	let born = s.points.first().map_or(f64::INFINITY, |p| p.t);
	if scraped && born > t + EDGE_SCRAPE_MS && node_first.is_some_and(|n| born > n + EDGE_SCRAPE_MS)
	{
		return 0.0;
	}
	let p = if scraped {
		s.points.iter().find(|p| p.t >= t).or(s.points.last())
	} else {
		s.points.iter().rev().find(|p| p.t <= t)
	};
	p.map_or(0.0, |p| p.value)
}

/// Histogram buckets over a window: (upper bound, count), ascending, `+Inf` last.
pub type Buckets = Vec<(f64, f64)>;

/// A finished run: its recorded series plus what the load tool knew.
#[derive(Debug)]
pub struct RunData {
	store: Store,
	/// summary.json without the checks.
	pub summary: Summary,
	/// The chain's block interval before the run.
	pub block_interval_s: f64,
	/// Each node's first scrape, by (job, instance).
	node_first: HashMap<(String, String), f64>,
}

impl RunData {
	/// From the run's series and summary.
	pub fn new(store: Store, summary: Summary) -> Self {
		let block_interval_s = summary.network.block_interval_s;
		let mut node_first: HashMap<(String, String), f64> = HashMap::new();
		for s in store.values().flatten() {
			if let Some(p) = s.points.first() {
				let t = node_first.entry(node(s)).or_insert(p.t);
				*t = t.min(p.t);
			}
		}
		Self { store, summary, block_interval_s, node_first }
	}

	fn value_at(&self, s: &Series, t: f64) -> f64 {
		value_at(s, t, self.node_first.get(&node(s)).copied())
	}

	/// Series of `name` whose labels match.
	pub fn series(&self, name: &str, filter: Filter<'_>) -> Vec<&Series> {
		let matches =
			|s: &&Series| filter.iter().all(|(k, v)| s.labels.get(*k).is_some_and(|x| x == v));
		self.store.get(name).into_iter().flatten().filter(matches).collect()
	}

	/// True when some series matches.
	pub fn has(&self, name: &str, filter: Filter<'_>) -> bool {
		!self.series(name, filter).is_empty()
	}

	/// The value at `t`, summed over matching series; `None` when none matches.
	pub fn at(&self, name: &str, filter: Filter<'_>, t: f64) -> Option<f64> {
		let list = self.series(name, filter);
		(!list.is_empty()).then(|| list.iter().map(|s| self.value_at(s, t)).sum())
	}

	/// One series' increase over `w`; a counter that went down means its node restarted.
	fn delta(&self, name: &str, s: &Series, w: &Window) -> Result<f64, CounterReset> {
		let (a, b) = (self.value_at(s, w.start), self.value_at(s, w.end));
		if b < a {
			return Err(CounterReset(format!("{name} went down from {a} to {b}")));
		}
		Ok(b - a)
	}

	/// A counter's increase over `w`, summed over matching series.
	pub fn diff(
		&self,
		name: &str,
		filter: Filter<'_>,
		w: &Window,
	) -> Result<Option<f64>, CounterReset> {
		let mut sum = None;
		for s in self.series(name, filter) {
			*sum.get_or_insert(0.0) += self.delta(name, s, w)?;
		}
		Ok(sum)
	}

	/// A counter's increase over `w`, summed over matching series; 0 when none matches.
	pub fn increase(
		&self,
		name: &str,
		filter: Filter<'_>,
		w: &Window,
	) -> Result<f64, CounterReset> {
		Ok(self.diff(name, filter, w)?.unwrap_or(0.0))
	}

	/// A histogram's bucket increases over `w`, summed over matching series.
	pub fn buckets(
		&self,
		name: &str,
		filter: Filter<'_>,
		w: &Window,
	) -> Result<Option<Buckets>, CounterReset> {
		let mut out: Buckets = Vec::new();
		for s in self.series(&format!("{name}_bucket"), filter) {
			let le = match s.labels.get("le").map(String::as_str) {
				Some("+Inf") => f64::INFINITY,
				Some(le) => le.parse().unwrap_or(f64::NAN),
				None => continue,
			};
			add_to(&mut out, le, self.delta(name, s, w)?);
		}
		out.sort_by(|a, b| a.0.total_cmp(&b.0));
		Ok((!out.is_empty()).then_some(out))
	}

	/// The ramp steps, from the `polkameter_step` gauge.
	pub fn steps(&self) -> Vec<Window> {
		let Some(s) = self.series("polkameter_step", &[]).first().copied() else {
			return Vec::new();
		};
		let p = &s.points;
		(0..p.len())
			.filter(|&i| p[i].value >= 0.0)
			.map(|i| Window {
				start: p[i].t,
				end: p.get(i + 1).map_or(p[i].t, |n| n.t),
				label: Label::Step(p[i].value as u32),
			})
			.collect()
	}

	/// The steps, then recovery: after a burst (one short step) the load lands there.
	pub fn load_windows(&self) -> Vec<Window> {
		let mut w = self.steps();
		w.extend(self.phase(Phase::Recovery));
		w
	}

	/// The window of a phase, from the `polkameter_phase` gauge. Takes a [`Phase`] or its name
	/// (`"done"`); `None` for a name no phase has, or a phase the run did not record.
	/// Part of the plugin API (used by out-of-tree plugins).
	pub fn phase(&self, phase: impl AsRef<str>) -> Option<Window> {
		let phase = Phase::from_name(phase.as_ref())?;
		let s = *self.series("polkameter_phase", &[("phase", phase.name())]).first()?;
		let on = s.points.iter().position(|p| p.value == 1.0)?;
		let end = s.points[on + 1..].iter().find(|p| p.value == 0.0).or(s.points.last())?;
		Some(Window { start: s.points[on].t, end: end.t, label: Label::Phase(phase) })
	}

	/// The whole run: first to last sample of `polkameter_phase`.
	pub fn run(&self) -> Window {
		let ts = self
			.series("polkameter_phase", &[])
			.into_iter()
			.flat_map(|s| s.points.iter().map(|p| p.t));
		let (start, end) =
			ts.fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), t| (a.min(t), b.max(t)));
		Window { start, end, label: Label::Run }
	}

	/// A gauge's lowest and highest value in `w`, summed over matching series (each series'
	/// own extremes; one without a sample inside takes its value at the start).
	/// Part of the plugin API (used by out-of-tree plugins).
	pub fn gauge_range(&self, name: &str, filter: Filter<'_>, w: &Window) -> Option<(f64, f64)> {
		let list = self.series(name, filter);
		if list.is_empty() {
			return None;
		}
		let (mut min, mut max) = (0.0, 0.0);
		for s in list {
			let inside: Vec<f64> = s
				.points
				.iter()
				.filter(|p| p.t >= w.start && p.t <= w.end)
				.map(|p| p.value)
				.collect();
			let values = if inside.is_empty() { vec![self.value_at(s, w.start)] } else { inside };
			min += values.iter().copied().fold(f64::INFINITY, f64::min);
			max += values.iter().copied().fold(f64::NEG_INFINITY, f64::max);
		}
		Some((min, max))
	}

	/// Every sample time of `name`, whatever the labels.
	/// Part of the plugin API (used by out-of-tree plugins).
	pub fn times(&self, name: &str) -> Vec<f64> {
		self.series(name, &[])
			.into_iter()
			.flat_map(|s| s.points.iter().map(|p| p.t))
			.collect()
	}

	/// The collator's blocks per end reason (short names) in `w`.
	pub fn end_reasons(&self, w: &Window) -> Result<Vec<(String, f64)>, CounterReset> {
		// One series per collator and reason; `diff` already sums the collators, so once per
		// reason.
		let mut reasons: Vec<&str> = self
			.series(END_REASON, &[COLLATOR])
			.iter()
			.filter_map(|s| s.labels.get("reason").map(String::as_str))
			.collect();
		reasons.sort_unstable();
		reasons.dedup();
		let mut out: Vec<(String, f64)> = Vec::new();
		for reason in reasons {
			let n = self.diff(END_REASON, &[COLLATOR, ("reason", reason)], w)?.unwrap_or(0.0);
			if n > 0.0 {
				add_to(&mut out, short_reason(reason).to_owned(), n);
			}
		}
		Ok(out)
	}
}

/// `reason count` pairs, comma-separated: `empty 3, weight 2`.
pub fn reasons_text(r: &[(String, f64)]) -> String {
	r.iter().map(|(k, n)| format!("{k} {}", num(*n))).collect::<Vec<_>>().join(", ")
}

/// Adds `n` to the entry for `key`, or appends one.
pub(crate) fn add_to<K: PartialEq, V: AddAssign>(list: &mut Vec<(K, V)>, key: K, n: V) {
	match list.iter_mut().find(|(k, _)| *k == key) {
		Some((_, sum)) => *sum += n,
		None => list.push((key, n)),
	}
}

fn short_reason(reason: &str) -> &str {
	match reason {
		"hit_block_weight_limit" => "weight",
		"hit_block_size_limit" => "size",
		"hit_deadline" => "deadline",
		"no_more_transactions" => "empty",
		"transactions_forbidden" => "forbidden",
		other => other,
	}
}

/// The bucket bound below which a share `q` of the counts fall; `None` with no counts.
pub fn quantile(b: Option<&Buckets>, q: f64) -> Option<f64> {
	let b = b?;
	let total = b
		.iter()
		.find(|(le, _)| le.is_infinite())
		.map_or_else(|| b.iter().map(|x| x.1).fold(0.0, f64::max), |x| x.1);
	if total == 0.0 {
		return None;
	}
	Some(b.iter().find(|(_, n)| *n >= q * total).map_or(f64::INFINITY, |x| x.0))
}

/// Counts above `bound` (one of the buckets).
pub fn count_above(b: Option<&Buckets>, bound: f64) -> Option<f64> {
	let b = b?;
	let get = |x: f64| b.iter().find(|(le, _)| *le == x).map_or(0.0, |x| x.1);
	Some(get(f64::INFINITY) - get(bound))
}
