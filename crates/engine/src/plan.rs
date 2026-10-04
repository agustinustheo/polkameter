use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub const NAMESPACE: &str = "https://polkameter.dev/schema/plan/v2";
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
	#[serde(rename = "@version")]
	pub version: u32,
	#[serde(rename = "@xmlns")]
	pub namespace: String,
	#[serde(rename = "@name")]
	pub name: String,
	#[serde(rename = "@timeout-ms", default = "run_timeout")]
	pub timeout_ms: u64,
	#[serde(rename = "@mode", default = "mode")]
	pub mode: String,
	#[serde(default)]
	pub plugins: Plugins,
	#[serde(default)]
	pub targets: Targets,
	#[serde(default)]
	pub credentials: Credentials,
	#[serde(default)]
	pub preflight: Steps,
	#[serde(default)]
	pub setup: Steps,
	#[serde(rename = "workflow", default)]
	pub workflows: Vec<Workflow>,
	pub load: Option<Load>,
	pub monitors: Option<Monitors>,
	#[serde(default)]
	pub thresholds: Thresholds,
	#[serde(default)]
	pub evaluate: Steps,
	#[serde(default)]
	pub teardown: Steps,
}
fn run_timeout() -> u64 {
	3_600_000
}
fn mode() -> String {
	"stress".into()
}
fn step_timeout() -> u64 {
	60_000
}
fn one() -> u32 {
	1
}
fn connections() -> usize {
	4
}
fn recovery() -> u32 {
	900
}
fn probes() -> usize {
	5
}
fn block_interval() -> f64 {
	6.0
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Plugins {
	#[serde(rename = "plugin", default)]
	pub entries: Vec<PluginSpec>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PluginSpec {
	#[serde(rename = "@id")]
	pub id: String,
	#[serde(rename = "@version")]
	pub version: String,
	#[serde(rename = "@protocol")]
	pub protocol: u32,
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Targets {
	#[serde(rename = "target", default)]
	pub entries: Vec<Target>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
	#[serde(rename = "@id")]
	pub id: String,
	#[serde(rename = "@endpoint")]
	pub endpoint: String,
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Credentials {
	#[serde(rename = "credential", default)]
	pub entries: Vec<Credential>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Credential {
	#[serde(rename = "@id")]
	pub id: String,
	#[serde(rename = "@profile")]
	pub profile: String,
}
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Steps {
	#[serde(rename = "step", default)]
	pub entries: Vec<Step>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
	#[serde(rename = "@id")]
	pub id: String,
	#[serde(rename = "@use")]
	pub operation: String,
	#[serde(rename = "@timeout-ms", default = "step_timeout")]
	pub timeout_ms: u64,
	#[serde(rename = "input", default)]
	pub inputs: Vec<Input>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
	#[serde(rename = "@name")]
	pub name: String,
	#[serde(rename = "@ref")]
	pub reference: Option<String>,
	#[serde(rename = "@value")]
	pub value: Option<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Workflow {
	#[serde(rename = "@id")]
	pub id: String,
	#[serde(rename = "@users", default = "one")]
	pub users: u32,
	#[serde(rename = "@iterations", default = "one")]
	pub iterations: u32,
	#[serde(rename = "@concurrency", default = "one")]
	pub concurrency: u32,
	#[serde(rename = "step", default)]
	pub steps: Vec<Step>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Load {
	#[serde(rename = "@target")]
	pub target: String,
	#[serde(rename = "@source-ref")]
	pub source: String,
	#[serde(rename = "@probes-ref")]
	pub probes_ref: String,
	#[serde(rename = "@state-check")]
	pub state_check: Option<String>,
	#[serde(rename = "@state-ref")]
	pub state_ref: Option<String>,
	#[serde(rename = "@connections", default = "connections")]
	pub connections: usize,
	#[serde(rename = "@baseline-probes", default = "probes")]
	pub baseline_probes: usize,
	#[serde(rename = "@recovery-seconds", default = "recovery")]
	pub recovery_seconds: u32,
	#[serde(rename = "@block-interval-seconds", default = "block_interval")]
	pub block_interval_seconds: f64,
	#[serde(rename = "rate", default)]
	pub rates: Vec<Rate>,
	pub ramp: Option<Ramp>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Rate {
	#[serde(rename = "@tx-per-second")]
	pub rate: f64,
	#[serde(rename = "@seconds")]
	pub seconds: u32,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Ramp {
	#[serde(rename = "@start")]
	pub start: f64,
	#[serde(rename = "@step")]
	pub step: Option<f64>,
	#[serde(rename = "@growth")]
	pub growth: Option<f64>,
	#[serde(rename = "@steps")]
	pub steps: u32,
	#[serde(rename = "@seconds")]
	pub seconds: u32,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Monitors {
	#[serde(rename = "metric", default)]
	pub metrics: Vec<MetricRequirement>,
	/// Alias into the executing host's topology registry.
	#[serde(rename = "@topology")]
	pub topology: String,
	#[serde(rename = "@relay-target")]
	pub relay_target: String,
}
impl Input {
	pub fn literal(&self) -> Result<Value> {
		let text = self.value.as_ref().ok_or_else(|| anyhow::anyhow!("missing literal"))?;
		Ok(serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.clone())))
	}
}
impl Load {
	pub fn rate_plan(&self) -> Result<polkameter_load::Plan> {
		ensure!(self.ramp.is_none() || self.rates.is_empty(), "use rates or a ramp, not both");
		let plan = if let Some(r) = &self.ramp {
			ensure!(
				r.steps <= 10_000 && (r.step.is_some() ^ r.growth.is_some()),
				"ramp requires step or growth and at most 10000 steps"
			);
			ensure!(r.start.is_finite() && r.start > 0.0, "invalid ramp start");
			if let Some(growth) = r.growth {
				ensure!(growth.is_finite() && growth > 0.0, "invalid growth");
				polkameter_load::Plan::geometric(r.start, growth, r.seconds, r.steps)
			} else {
				polkameter_load::Plan::ramp(r.start, r.step.unwrap(), r.seconds, r.steps)
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
		// Reject doctypes and enforce the root before serde ignores its name.
		let mut reader = quick_xml::Reader::from_str(xml);
		let mut root = false;
		loop {
			match reader.read_event()? {
				quick_xml::events::Event::DocType(_) => anyhow::bail!("DOCTYPE is not supported"),
				quick_xml::events::Event::Start(e) | quick_xml::events::Event::Empty(e)
					if !root =>
				{
					ensure!(
						e.name().as_ref() == b"polkameter-plan",
						"expected polkameter-plan root"
					);
					root = true;
				},
				quick_xml::events::Event::Eof => break,
				_ => {},
			}
		}
		let plan: Self = quick_xml::de::from_str(xml)?;
		plan.validate()?;
		Ok(plan)
	}
	pub fn validate(&self) -> Result<()> {
		self.thresholds.rules()?;
		ensure!(self.version == 2 && self.namespace == NAMESPACE, "expected XML plan v2 namespace");
		ensure!(!self.name.is_empty() && self.timeout_ms > 0, "name and run deadline required");
		ensure!(matches!(self.mode.as_str(), "stress" | "smoke"), "unknown run mode");
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
		let mut known: BTreeSet<String> = ["run.id", "run.probeBudget"].map(str::to_owned).into();
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
		self.validate_steps(&self.preflight.entries, &mut known, &mut ids, &plugin_ids)?;
		self.validate_steps(&self.setup.entries, &mut known, &mut ids, &plugin_ids)?;
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
			let mut local = known.clone();
			local.extend(["user.index".into(), "iteration.index".into()]);
			self.validate_steps(&workflow.steps, &mut local, &mut ids, &plugin_ids)?;
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
			ensure!(
				load.state_check.is_some() == load.state_ref.is_some(),
				"state check and reference must be specified together"
			);
			if let Some(reference) = &load.state_ref {
				ensure!(reference_known(&known, reference), "unknown state reference");
			}
			if let Some(operation) = &load.state_check {
				validate_operation(operation, &plugin_ids)?;
			}
		}
		if let Some(monitors) = &self.monitors {
			ensure!(
				self.load.is_some()
					&& known.contains(&format!("targets.{}", monitors.relay_target)),
				"monitors require load and relay target"
			);
		}
		self.validate_steps(&self.evaluate.entries, &mut known, &mut ids, &plugin_ids)?;
		self.validate_steps(&self.teardown.entries, &mut known, &mut ids, &plugin_ids)?;
		Ok(())
	}
	fn validate_steps(
		&self,
		steps: &[Step],
		known: &mut BTreeSet<String>,
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
				ensure!(
					names.insert(&input.name)
						&& (input.reference.is_some() ^ input.value.is_some()),
					"input needs exactly one ref or value"
				);
				if let Some(reference) = &input.reference {
					ensure!(
						reference_known(known, reference),
						"unknown or forward reference {reference}"
					);
				}
			}
			known.insert(format!("steps.{}", step.id));
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

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Thresholds {
	#[serde(rename = "@stall-ms")]
	pub stall_ms: Option<u64>,
	#[serde(rename = "@finality-stall-ms")]
	pub finality_stall_ms: Option<u64>,
	#[serde(rename = "@max-p95-latency-ms")]
	pub max_p95_latency_ms: Option<u64>,
	#[serde(rename = "@max-submit-reply-ms")]
	pub max_submit_reply_ms: Option<u64>,
	#[serde(rename = "@min-included-ratio")]
	pub min_included_ratio: Option<f64>,
	#[serde(rename = "@max-refused-ratio")]
	pub max_refused_ratio: Option<f64>,
	#[serde(rename = "@pool-refuses-ratio")]
	pub pool_refuses_ratio: Option<f64>,
	#[serde(rename = "@min-send-ratio")]
	pub min_send_ratio: Option<f64>,
	#[serde(rename = "@max-block-gap-factor")]
	pub max_block_gap_factor: Option<f64>,
	#[serde(rename = "@probes-in-a-row")]
	pub probes_in_a_row: Option<usize>,
	#[serde(rename = "@recovered-block-gap-factor")]
	pub recovered_block_gap_factor: Option<f64>,
	#[serde(rename = "@finality-wait-ms")]
	pub finality_wait_ms: Option<u64>,
	#[serde(rename = "@recycler-drain-ms")]
	pub recycler_drain_ms: Option<u64>,
}
impl Thresholds {
	pub fn rules(&self) -> Result<polkameter_load::rules::Rules> {
		let mut rules = polkameter_load::rules::RULES;
		if let Some(value) = self.stall_ms {
			ensure!(value > 0, "invalid stall-ms");
			rules.stall_ms = value;
		}
		if let Some(value) = self.finality_stall_ms {
			ensure!(value > 0, "invalid finality-stall-ms");
			rules.finality_stall_ms = value;
		}
		if let Some(value) = self.max_p95_latency_ms {
			ensure!(value > 0, "invalid max-p95-latency-ms");
			rules.max_p95_latency_ms = value;
		}
		if let Some(value) = self.max_submit_reply_ms {
			ensure!(value > 0, "invalid max-submit-reply-ms");
			rules.max_submit_reply_ms = value;
		}
		if let Some(value) = self.min_included_ratio {
			ensure!(value.is_finite() && value > 0.0 && value <= 1.0, "invalid min-included-ratio");
			rules.min_included_ratio = value;
		}
		if let Some(value) = self.max_refused_ratio {
			ensure!(value.is_finite() && value > 0.0 && value <= 1.0, "invalid max-refused-ratio");
			rules.max_refused_ratio = value;
		}
		if let Some(value) = self.pool_refuses_ratio {
			ensure!(value.is_finite() && value > 0.0 && value <= 1.0, "invalid pool-refuses-ratio");
			rules.pool_refuses_ratio = value;
		}
		if let Some(value) = self.min_send_ratio {
			ensure!(value.is_finite() && value > 0.0 && value <= 1.0, "invalid min-send-ratio");
			rules.min_send_ratio = value;
		}
		if let Some(value) = self.max_block_gap_factor {
			ensure!(value.is_finite() && value > 0.0, "invalid max-block-gap-factor");
			rules.max_block_gap_factor = value;
		}
		if let Some(value) = self.probes_in_a_row {
			ensure!(value > 0, "invalid probes-in-a-row");
			rules.probes_in_a_row = value;
		}
		if let Some(value) = self.recovered_block_gap_factor {
			ensure!(value.is_finite() && value > 0.0, "invalid recovered-block-gap-factor");
			rules.recovered_block_gap_factor = value;
		}
		if let Some(value) = self.finality_wait_ms {
			ensure!(value > 0, "invalid finality-wait-ms");
			rules.finality_wait_ms = value;
		}
		if let Some(value) = self.recycler_drain_ms {
			ensure!(value > 0, "invalid recycler-drain-ms");
			rules.recycler_drain_ms = value;
		}
		Ok(rules)
	}
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct MetricRequirement {
	#[serde(rename = "@role")]
	pub role: String,
	#[serde(rename = "@name")]
	pub name: String,
}
