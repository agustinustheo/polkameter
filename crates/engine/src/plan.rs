use crate::xml::{self, Element, error_at};
use anyhow::{Result, bail, ensure};
use polkameter_load::runner::Mode;
use serde::{Serialize, Serializer, ser::SerializeMap};
use serde_json::Value;
use std::{
	collections::{BTreeMap, BTreeSet},
	fmt::Display,
	str::FromStr,
};

pub const NAMESPACE: &str = "https://polkameter.dev/schema/plan";

/// Reads a plan element from XML; implemented by `element!` and by hand for the special cases.
pub trait FromXml: Sized {
	fn from_xml(el: &Element<'_>) -> Result<Self>;
}

/// One field's reader. `$at` and `$kids` record the names it consumes, for `Element::finish`.
macro_rules! read {
	($el:ident, $at:ident, $kids:ident, attr $a:literal) => {{
		$at.push($a);
		$el.req($a)?
	}};
	($el:ident, $at:ident, $kids:ident, attr $a:literal or $d:expr) => {{
		$at.push($a);
		$el.opt($a)?.unwrap_or($d)
	}};
	($el:ident, $at:ident, $kids:ident, opt_attr $a:literal) => {{
		$at.push($a);
		$el.opt($a)?
	}};
	($el:ident, $at:ident, $kids:ident, list $c:literal) => {{
		$kids.push($c);
		$el.all($c)?
	}};
	($el:ident, $at:ident, $kids:ident, opt_child $c:literal) => {{
		$kids.push($c);
		$el.one($c)?
	}};
	($el:ident, $at:ident, $kids:ident, child $c:literal) => {{
		$kids.push($c);
		$el.section($c)?
	}};
}
/// A field's JSON key: `@name` for an attribute, the element name for a child.
macro_rules! key {
	(attr $a:literal) => {
		concat!("@", $a)
	};
	(opt_attr $a:literal) => {
		concat!("@", $a)
	};
	($kind:ident $c:literal) => {
		$c
	};
}
/// Declares an element once: its struct, its XML reader and its JSON form.
macro_rules! element {
	($(#[$m:meta])* pub struct $name:ident {
		$($(#[$fm:meta])* $f:ident : $t:ty = $k:ident $a:literal $(or $d:expr)?),* $(,)?
	}) => {
		$(#[$m])*
		#[derive(Clone, Debug)]
		pub struct $name {
			$($(#[$fm])* pub $f: $t),*
		}
		impl FromXml for $name {
			fn from_xml(el: &Element<'_>) -> Result<Self> {
				#[allow(unused_mut)]
				let (mut at, mut kids): (Vec<&str>, Vec<&str>) = Default::default();
				let value = Self { $($f: read!(el, at, kids, $k $a $(or $d)?)),* };
				el.finish(&at, &kids)?;
				Ok(value)
			}
		}
		impl Serialize for $name {
			fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
				let mut map = s.serialize_map(None)?;
				$(map.serialize_entry(key!($k $a), &self.$f)?;)*
				map.end()
			}
		}
	};
}

/// An attribute's type. Numbers ignore surrounding whitespace, as they did before; strings and
/// run modes are taken exactly as written.
trait Attr: Sized {
	fn parse_attr(text: &str) -> std::result::Result<Self, String>;
}
impl Attr for String {
	fn parse_attr(text: &str) -> std::result::Result<Self, String> {
		Ok(text.to_owned())
	}
}
impl Attr for Mode {
	fn parse_attr(text: &str) -> std::result::Result<Self, String> {
		text.parse()
	}
}
macro_rules! numeric_attr {
	($($t:ty)*) => {$(
		impl Attr for $t {
			fn parse_attr(text: &str) -> std::result::Result<Self, String> {
				text.trim_matches([' ', '\t', '\r', '\n'])
					.parse()
					.map_err(|e: <$t as FromStr>::Err| e.to_string())
			}
		}
	)*};
}
numeric_attr!(u32 u64 usize f64);

impl Element<'_> {
	fn err(&self, msg: impl Display) -> anyhow::Error {
		error_at(self.src, self.offset, msg)
	}
	fn opt<T: Attr>(&self, name: &str) -> Result<Option<T>> {
		let Some((_, value, at)) = self.attrs.iter().find(|a| a.0 == name) else {
			return Ok(None);
		};
		let parsed = T::parse_attr(value)
			.map_err(|e| error_at(self.src, *at, format!("invalid `{name}`: {e}")))?;
		Ok(Some(parsed))
	}
	fn req<T: Attr>(&self, name: &str) -> Result<T> {
		self.opt(name)?
			.ok_or_else(|| self.err(format!("missing `{name}` on <{}>", self.name)))
	}
	fn all<T: FromXml>(&self, name: &str) -> Result<Vec<T>> {
		self.children.iter().filter(|c| c.name == name).map(T::from_xml).collect()
	}
	fn one<T: FromXml>(&self, name: &str) -> Result<Option<T>> {
		let found: Vec<_> = self.children.iter().filter(|c| c.name == name).collect();
		match found[..] {
			[] => Ok(None),
			[child] => T::from_xml(child).map(Some),
			[_, extra, ..] => {
				let msg = format!("<{name}> may appear once in <{}>", self.name);
				Err(error_at(self.src, extra.offset, msg))
			},
		}
	}
	fn section<T: FromXml + Default>(&self, name: &str) -> Result<T> {
		Ok(self.one(name)?.unwrap_or_default())
	}
	/// Rejects attributes and children that no reader consumed; `attrs` and `kids` list them.
	fn finish(&self, attrs: &[&str], kids: &[&str]) -> Result<()> {
		if let Some((key, _, at)) = self.attrs.iter().find(|a| !attrs.contains(&a.0)) {
			let msg = format!("unknown attribute `{key}` on <{}>", self.name);
			return Err(error_at(self.src, *at, msg));
		}
		if let Some(child) = self.children.iter().find(|c| !kids.contains(&c.name)) {
			let msg = format!("unknown element <{}> in <{}>", child.name, self.name);
			return Err(error_at(self.src, child.offset, msg));
		}
		Ok(())
	}
}

element! {
	pub struct Plan {
		version: u32 = attr "version",
		namespace: String = attr "xmlns",
		name: String = attr "name",
		timeout_ms: u64 = attr "timeout-ms" or 3_600_000,
		mode: Mode = attr "mode" or Mode::Stress,
		plugins: Plugins = child "plugins",
		targets: Targets = child "targets",
		credentials: Credentials = child "credentials",
		preflight: Steps = child "preflight",
		setup: Steps = child "setup",
		workflows: Vec<Workflow> = list "workflow",
		load: Option<Load> = opt_child "load",
		monitors: Option<Monitors> = opt_child "monitors",
		thresholds: Thresholds = child "thresholds",
		evaluate: Steps = child "evaluate",
		teardown: Steps = child "teardown",
	}
}
element! {
	#[derive(Default)]
	pub struct Plugins {
		entries: Vec<PluginSpec> = list "plugin",
	}
}
element! {
	pub struct PluginSpec {
		id: String = attr "id",
		version: String = attr "version",
		protocol: u32 = attr "protocol",
	}
}
element! {
	#[derive(Default)]
	pub struct Targets {
		entries: Vec<Target> = list "target",
	}
}
element! {
	pub struct Target {
		id: String = attr "id",
		endpoint: String = attr "endpoint",
	}
}
element! {
	#[derive(Default)]
	pub struct Credentials {
		entries: Vec<Credential> = list "credential",
	}
}
element! {
	pub struct Credential {
		id: String = attr "id",
		profile: String = attr "profile",
	}
}
element! {
	#[derive(Default)]
	pub struct Steps {
		entries: Vec<Step> = list "step",
	}
}
element! {
	pub struct Step {
		id: String = attr "id",
		/// `plugin.operation`; `Plan::validate` checks the form and the provider.
		operation: String = attr "use",
		timeout_ms: u64 = attr "timeout-ms" or 60_000,
		inputs: Vec<Input> = list "input",
	}
}
element! {
	pub struct Workflow {
		id: String = attr "id",
		users: u32 = attr "users" or 1,
		iterations: u32 = attr "iterations" or 1,
		concurrency: u32 = attr "concurrency" or 1,
		steps: Vec<Step> = list "step",
	}
}
element! {
	pub struct Monitors {
		metrics: Vec<MetricRequirement> = list "metric",
		/// Alias into the executing host's topology registry.
		topology: String = attr "topology",
		/// The parachain whose collators are scraped and whose relay slots are judged.
		para_id: u32 = attr "para-id",
		relay_target: String = attr "relay-target",
	}
}
element! {
	pub struct MetricRequirement {
		role: String = attr "role",
		name: String = attr "name",
	}
}
element! {
	pub struct Rate {
		rate: f64 = attr "tx-per-second",
		seconds: u32 = attr "seconds",
	}
}
element! {
	#[derive(Default)]
	pub struct Thresholds {
		stall_ms: Option<u64> = opt_attr "stall-ms",
		finality_stall_ms: Option<u64> = opt_attr "finality-stall-ms",
		max_p95_latency_ms: Option<u64> = opt_attr "max-p95-latency-ms",
		max_submit_reply_ms: Option<u64> = opt_attr "max-submit-reply-ms",
		min_included_ratio: Option<f64> = opt_attr "min-included-ratio",
		max_refused_ratio: Option<f64> = opt_attr "max-refused-ratio",
		pool_refuses_ratio: Option<f64> = opt_attr "pool-refuses-ratio",
		min_send_ratio: Option<f64> = opt_attr "min-send-ratio",
		max_block_gap_factor: Option<f64> = opt_attr "max-block-gap-factor",
		probes_in_a_row: Option<usize> = opt_attr "probes-in-a-row",
		recovered_block_gap_factor: Option<f64> = opt_attr "recovered-block-gap-factor",
		finality_wait_ms: Option<u64> = opt_attr "finality-wait-ms",
	}
}
/// One input of a step: a reference to an earlier output, or a literal. Exactly one is given.
#[derive(Clone, Debug)]
pub struct Input {
	pub name: String,
	pub source: InputSource,
}
#[derive(Clone, Debug)]
pub enum InputSource {
	/// A reference to an earlier output, such as `steps.seed.value`.
	Ref(String),
	/// A literal; JSON when it parses, otherwise a string.
	Value(String),
}
impl FromXml for Input {
	fn from_xml(el: &Element<'_>) -> Result<Self> {
		let source = match (el.opt("ref")?, el.opt("value")?) {
			(Some(reference), None) => InputSource::Ref(reference),
			(None, Some(value)) => InputSource::Value(value),
			_ => return Err(el.err("input needs exactly one of ref or value")),
		};
		el.finish(&["name", "ref", "value"], &[])?;
		Ok(Input { name: el.req("name")?, source })
	}
}
impl Serialize for Input {
	fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
		let mut map = s.serialize_map(None)?;
		map.serialize_entry("@name", &self.name)?;
		let (reference, value) = match &self.source {
			InputSource::Ref(reference) => (Some(reference), None),
			InputSource::Value(value) => (None, Some(value)),
		};
		map.serialize_entry("@ref", &reference)?;
		map.serialize_entry("@value", &value)?;
		map.end()
	}
}

/// A ramp: `start` transactions per second, grown by `step` or by a factor, for `steps` steps.
#[derive(Clone, Debug)]
pub struct Ramp {
	pub start: f64,
	pub growth: RampGrowth,
	pub steps: u32,
	pub seconds: u32,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RampGrowth {
	/// Adds this many transactions per second at each step.
	Step(f64),
	/// Multiplies the rate by this factor at each step.
	Factor(f64),
}
impl FromXml for Ramp {
	fn from_xml(el: &Element<'_>) -> Result<Self> {
		let growth = match (el.opt("step")?, el.opt("growth")?) {
			(Some(step), None) => RampGrowth::Step(step),
			(None, Some(factor)) => RampGrowth::Factor(factor),
			_ => return Err(el.err("ramp needs exactly one of step or growth")),
		};
		el.finish(&["start", "step", "growth", "steps", "seconds"], &[])?;
		Ok(Ramp {
			start: el.req("start")?,
			growth,
			steps: el.req("steps")?,
			seconds: el.req("seconds")?,
		})
	}
}
impl Serialize for Ramp {
	fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
		let mut map = s.serialize_map(None)?;
		map.serialize_entry("@start", &self.start)?;
		let (step, growth) = match self.growth {
			RampGrowth::Step(step) => (Some(step), None),
			RampGrowth::Factor(growth) => (None, Some(growth)),
		};
		map.serialize_entry("@step", &step)?;
		map.serialize_entry("@growth", &growth)?;
		map.serialize_entry("@steps", &self.steps)?;
		map.serialize_entry("@seconds", &self.seconds)?;
		map.end()
	}
}

/// A state check operation and the reference it is given; both or neither appear in a plan.
#[derive(Clone, Debug)]
pub struct StateCheck {
	pub check: String,
	pub reference: String,
}
#[derive(Clone, Debug)]
pub struct Load {
	pub target: String,
	pub source: String,
	pub probes_ref: String,
	pub state: Option<StateCheck>,
	pub connections: usize,
	pub baseline_probes: usize,
	pub recovery_seconds: u32,
	pub block_interval_seconds: f64,
	pub rates: Vec<Rate>,
	pub ramp: Option<Ramp>,
}
impl Serialize for Load {
	fn serialize<S: Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
		let (check, reference) = match &self.state {
			Some(state) => (Some(&state.check), Some(&state.reference)),
			None => (None, None),
		};
		let mut map = s.serialize_map(None)?;
		map.serialize_entry("@target", &self.target)?;
		map.serialize_entry("@source-ref", &self.source)?;
		map.serialize_entry("@probes-ref", &self.probes_ref)?;
		map.serialize_entry("@state-check", &check)?;
		map.serialize_entry("@state-ref", &reference)?;
		map.serialize_entry("@connections", &self.connections)?;
		map.serialize_entry("@baseline-probes", &self.baseline_probes)?;
		map.serialize_entry("@recovery-seconds", &self.recovery_seconds)?;
		map.serialize_entry("@block-interval-seconds", &self.block_interval_seconds)?;
		map.serialize_entry("rate", &self.rates)?;
		map.serialize_entry("ramp", &self.ramp)?;
		map.end()
	}
}
impl FromXml for Load {
	fn from_xml(el: &Element<'_>) -> Result<Self> {
		let state = match (el.opt("state-check")?, el.opt("state-ref")?) {
			(Some(check), Some(reference)) => Some(StateCheck { check, reference }),
			(None, None) => None,
			_ => return Err(el.err("state-check and state-ref must be given together")),
		};
		el.finish(
			&[
				"target",
				"source-ref",
				"probes-ref",
				"state-check",
				"state-ref",
				"connections",
				"baseline-probes",
				"recovery-seconds",
				"block-interval-seconds",
			],
			&["rate", "ramp"],
		)?;
		Ok(Load {
			target: el.req("target")?,
			source: el.req("source-ref")?,
			probes_ref: el.req("probes-ref")?,
			state,
			connections: el.opt("connections")?.unwrap_or(4),
			baseline_probes: el.opt("baseline-probes")?.unwrap_or(5),
			recovery_seconds: el.opt("recovery-seconds")?.unwrap_or(900),
			block_interval_seconds: el.opt("block-interval-seconds")?.unwrap_or(6.0),
			rates: el.all("rate")?,
			ramp: el.one("ramp")?,
		})
	}
}

impl Input {
	pub fn literal(&self) -> Result<Value> {
		let InputSource::Value(text) = &self.source else { bail!("missing literal") };
		Ok(serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.clone())))
	}
}
impl Load {
	pub fn rate_plan(&self) -> Result<polkameter_load::Plan> {
		ensure!(self.ramp.is_none() || self.rates.is_empty(), "use rates or a ramp, not both");
		let plan = if let Some(r) = &self.ramp {
			ensure!(r.steps <= 10_000, "ramp requires at most 10000 steps");
			ensure!(r.start.is_finite() && r.start > 0.0, "invalid ramp start");
			match r.growth {
				RampGrowth::Factor(growth) => {
					ensure!(growth.is_finite() && growth > 0.0, "invalid growth");
					polkameter_load::Plan::geometric(r.start, growth, r.seconds, r.steps)
				},
				RampGrowth::Step(step) => {
					polkameter_load::Plan::ramp(r.start, step, r.seconds, r.steps)
				},
			}
		} else {
			polkameter_load::Plan {
				steps: self
					.rates
					.iter()
					.map(|r| polkameter_load::plan::StepPlan {
						seconds: r.seconds,
						rates: vec![r.rate],
					})
					.collect(),
			}
		};
		plan.check(Some(1)).map_err(anyhow::Error::msg)?;
		ensure!(plan.steps.len() <= 10_000, "too many rate steps");
		Ok(plan)
	}
}
impl Plan {
	pub fn parse(xml: &str) -> Result<Self> {
		ensure!(xml.len() <= 2 * 1024 * 1024, "XML plan exceeds 2 MiB");
		let root = xml::parse(xml)?;
		if root.name != "polkameter-plan" {
			return Err(root.err("expected polkameter-plan root"));
		}
		let plan = Plan::from_xml(&root)?;
		plan.validate()?;
		Ok(plan)
	}
	pub fn validate(&self) -> Result<()> {
		self.thresholds.rules()?;
		ensure!(
			self.version == 1 && self.namespace == NAMESPACE,
			"expected a version 1 Polkameter plan in its namespace"
		);
		ensure!(!self.name.is_empty() && self.timeout_ms > 0, "name and run deadline required");
		let mut plugin_ids = BTreeSet::new();
		for p in &self.plugins.entries {
			ensure!(
				identifier(&p.id) && p.id != "core" && plugin_ids.insert(&p.id),
				"duplicate or invalid plugin ID"
			);
			ensure!(
				p.protocol == 1 && !p.version.is_empty(),
				"unsupported plugin protocol or empty version"
			);
		}
		let mut known: BTreeSet<String> =
			["run.id", "run.probeBudget", "run.directory"].map(str::to_owned).into();
		for t in &self.targets.entries {
			ensure!(
				identifier(&t.id) && known.insert(format!("targets.{}", t.id)),
				"duplicate or invalid target"
			);
			ensure!(
				t.endpoint.starts_with("ws://") || t.endpoint.starts_with("wss://"),
				"target must be WebSocket RPC"
			);
		}
		for c in &self.credentials.entries {
			ensure!(
				identifier(&c.id)
					&& known.insert(format!("credentials.{}", c.id))
					&& !c.profile.is_empty(),
				"duplicate or invalid credential"
			);
		}
		let mut ids = BTreeSet::new();
		self.validate_plan_steps(&self.preflight.entries, &mut known, &mut ids, &plugin_ids)?;
		self.validate_plan_steps(&self.setup.entries, &mut known, &mut ids, &plugin_ids)?;
		let mut workflow_ids = BTreeSet::new();
		for workflow in &self.workflows {
			ensure!(
				identifier(&workflow.id) && workflow_ids.insert(&workflow.id),
				"duplicate workflow"
			);
			ensure!(
				workflow.users > 0
					&& workflow.users <= 100_000
					&& workflow.iterations > 0
					&& workflow.concurrency > 0
					&& workflow.concurrency <= 1000,
				"invalid workflow limits"
			);
			// A workflow's own outputs and loop indices are visible only inside it.
			let mut scope = BTreeSet::from(["user.index".to_owned(), "iteration.index".to_owned()]);
			self.validate_steps(&workflow.steps, &known, &mut scope, &mut ids, &plugin_ids)?;
		}
		if let Some(load) = &self.load {
			load.rate_plan()?;
			ensure!(known.contains(&format!("targets.{}", load.target)), "unknown load target");
			ensure!(
				reference_known(&known, &load.source) && reference_known(&known, &load.probes_ref),
				"unknown workload reference"
			);
			ensure!(
				load.connections > 0
					&& load.connections <= 64
					&& load.baseline_probes > 0
					&& load.baseline_probes <= 1000
					&& load.recovery_seconds <= 86_400,
				"invalid load limits"
			);
			ensure!(
				load.block_interval_seconds.is_finite() && load.block_interval_seconds >= 0.1,
				"invalid expected block interval"
			);
			if let Some(state) = &load.state {
				ensure!(reference_known(&known, &state.reference), "unknown state reference");
				validate_operation(&state.check, &plugin_ids)?;
			}
		}
		if let Some(monitors) = &self.monitors {
			ensure!(
				self.load.is_some()
					&& known.contains(&format!("targets.{}", monitors.relay_target)),
				"monitors require load and relay target"
			);
		}
		self.validate_plan_steps(&self.evaluate.entries, &mut known, &mut ids, &plugin_ids)?;
		self.validate_plan_steps(&self.teardown.entries, &mut known, &mut ids, &plugin_ids)?;
		Ok(())
	}
	/// Checks plan-level steps, whose outputs stay visible to the steps after them.
	fn validate_plan_steps(
		&self,
		steps: &[Step],
		known: &mut BTreeSet<String>,
		ids: &mut BTreeSet<String>,
		plugins: &BTreeSet<&String>,
	) -> Result<()> {
		let mut outputs = BTreeSet::new();
		self.validate_steps(steps, known, &mut outputs, ids, plugins)?;
		known.append(&mut outputs);
		Ok(())
	}
	/// Checks steps in order. References may name `known` (the plan's values and earlier steps
	/// outside workflows) or `scope` (this scope's own step outputs); outputs go into `scope`.
	fn validate_steps(
		&self,
		steps: &[Step],
		known: &BTreeSet<String>,
		scope: &mut BTreeSet<String>,
		ids: &mut BTreeSet<String>,
		plugins: &BTreeSet<&String>,
	) -> Result<()> {
		for step in steps {
			ensure!(
				identifier(&step.id) && ids.insert(step.id.clone()) && step.timeout_ms > 0,
				"invalid or duplicate step {}",
				step.id
			);
			validate_operation(&step.operation, plugins)?;
			let mut names = BTreeSet::new();
			for input in &step.inputs {
				ensure!(names.insert(&input.name), "duplicate input {}", input.name);
				if let InputSource::Ref(reference) = &input.source {
					ensure!(
						reference_known(known, reference) || reference_known(scope, reference),
						"unknown or forward reference {reference}"
					);
				}
			}
			scope.insert(format!("steps.{}", step.id));
		}
		Ok(())
	}
	pub fn all_steps(&self) -> impl Iterator<Item = &Step> {
		self.preflight
			.entries
			.iter()
			.chain(&self.setup.entries)
			.chain(self.workflows.iter().flat_map(|w| &w.steps))
			.chain(&self.evaluate.entries)
			.chain(&self.teardown.entries)
	}
	pub fn values(&self, run_id: &str) -> BTreeMap<String, Value> {
		let mut values: BTreeMap<_, _> = self
			.targets
			.entries
			.iter()
			.map(|t| (format!("targets.{}", t.id), Value::String(t.endpoint.clone())))
			.collect();
		values.insert("run.id".into(), Value::String(run_id.into()));
		// The run's directory; the run sets it once the directory exists.
		values.insert("run.directory".into(), Value::String(String::new()));
		let probes = self.load.as_ref().map_or(0, |load| {
			polkameter_load::probe_count(
				load.baseline_probes,
				load.recovery_seconds,
				load.block_interval_seconds,
			)
		});
		values.insert("run.probeBudget".into(), Value::from(probes));
		values
	}
}
fn identifier(id: &str) -> bool {
	!id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}
fn reference_known(known: &BTreeSet<String>, reference: &str) -> bool {
	known.contains(reference)
		|| reference.rsplit_once('.').is_some_and(|(prefix, _)| known.contains(prefix))
}
fn validate_operation(operation: &str, plugins: &BTreeSet<&String>) -> Result<()> {
	let (plugin, op) = operation
		.split_once('.')
		.ok_or_else(|| anyhow::anyhow!("operation must be plugin.name"))?;
	ensure!(
		identifier(op) && (plugin == "core" || plugins.iter().any(|p| p.as_str() == plugin)),
		"unknown operation provider {plugin}"
	);
	Ok(())
}

fn positive<T: Default + PartialOrd>(value: T) -> bool {
	value > T::default()
}
fn ratio(value: f64) -> bool {
	value.is_finite() && value > 0.0 && value <= 1.0
}
fn factor(value: f64) -> bool {
	value.is_finite() && value > 0.0
}
/// Overrides each rule the plan sets, after checking the value with its predicate.
macro_rules! overrides {
	($plan:ident, $rules:ident, $($field:ident: $check:ident = $name:literal),* $(,)?) => {
		$(if let Some(value) = $plan.$field {
			ensure!($check(value), "invalid {}", $name);
			$rules.$field = value;
		})*
	};
}
impl Thresholds {
	pub fn rules(&self) -> Result<polkameter_load::rules::Rules> {
		let mut rules = polkameter_load::rules::RULES;
		overrides!(self, rules,
			stall_ms: positive = "stall-ms",
			finality_stall_ms: positive = "finality-stall-ms",
			max_p95_latency_ms: positive = "max-p95-latency-ms",
			max_submit_reply_ms: positive = "max-submit-reply-ms",
			min_included_ratio: ratio = "min-included-ratio",
			max_refused_ratio: ratio = "max-refused-ratio",
			pool_refuses_ratio: ratio = "pool-refuses-ratio",
			min_send_ratio: ratio = "min-send-ratio",
			max_block_gap_factor: factor = "max-block-gap-factor",
			probes_in_a_row: positive = "probes-in-a-row",
			recovered_block_gap_factor: factor = "recovered-block-gap-factor",
			finality_wait_ms: positive = "finality-wait-ms",
		);
		Ok(rules)
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn plan(attrs: &str) -> String {
		format!(r#"<polkameter-plan xmlns="{NAMESPACE}" version="1" name="x" {attrs}/>"#)
	}

	#[test]
	fn accepts_byte_order_mark_and_numbers_with_spaces() {
		let bom = format!("\u{feff}{}", plan(""));
		assert_eq!(Plan::parse(&bom).unwrap().timeout_ms, 3_600_000);
		assert_eq!(Plan::parse(&plan(r#"timeout-ms=" 7""#)).unwrap().timeout_ms, 7);
	}

	#[test]
	fn keeps_string_attributes_exact() {
		let parsed = Plan::parse(&plan(r#"mode="smoke""#).replace("name=\"x\"", "name=\" x \""));
		assert_eq!(parsed.unwrap().name, " x ");
		assert!(Plan::parse(&plan(r#"mode=" smoke""#)).is_err());
	}

	#[test]
	fn rejects_non_ascii_whitespace_around_numbers() {
		let xml = plan("timeout-ms=\"\u{a0}7\"");
		let error = Plan::parse(&xml).unwrap_err().to_string();
		assert!(error.contains("invalid `timeout-ms`"), "{error}");
		assert_eq!(Plan::parse(&plan("timeout-ms=\"\t7\r\n\"")).unwrap().timeout_ms, 7);
	}

	#[test]
	fn rejects_non_xml_character_references() {
		for reference in ["&#0;", "&#1;", "&#xB;", "&#xFFFE;"] {
			let xml = plan(&format!(r#"timeout-ms="{reference}""#));
			let error = Plan::parse(&xml).unwrap_err().to_string();
			assert!(error.contains("invalid character reference"), "{reference}: {error}");
		}
	}
}
