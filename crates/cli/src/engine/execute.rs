//! Execution of declarative steps through the same host in every frontend.
use crate::engine::monitors::Job;
use crate::engine::{
	artifacts, measurement,
	plan::{InputSource, Plan, Step},
	plugins::{Plugins, Registry, checksum},
	wiring,
};
use crate::redact::Redactor;
use anyhow::{Context as _, Result, bail, ensure};
use polkameter_plugin_sdk::chain::{ChainError, Client, fetch};
use polkameter_plugin_sdk::observers::RunDir;
use polkameter_plugin_sdk::{Artifact, Context, Operation, PreparedTx, Schema, strip_0x};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
	borrow::Cow,
	collections::{BTreeMap, BTreeSet},
	fs::{File, OpenOptions},
	future::Future,
	io::Write,
	path::{Path, PathBuf},
	sync::{Arc, Mutex},
	time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;

pub type Values = BTreeMap<String, Value>;
pub type EventSink = Arc<dyn Fn(Value) + Send + Sync>;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Outcome {
	pub artifact_dir: PathBuf,
	pub exit_code: i32,
	pub state: RunState,
	pub error: Option<String>,
	/// How teardown ended; absent when it did not run: the plugins never started, or the run
	/// failed before its setup phase, so there was nothing to tear down.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub teardown: Option<Teardown>,
	/// The phase a failed run failed in, such as `preflight` or `setup`. Absent otherwise.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub failed_phase: Option<String>,
	/// Where in the plan `error` points, when it is about one element. `error` then starts with
	/// `line L, column C: `.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub error_location: Option<crate::engine::plan::Location>,
	/// Why a `completed_with_failures` run exits 1: smoke-mode problems, or the verdict and the
	/// flag that failed it. Absent in every other state.
	#[serde(default, skip_serializing_if = "Option::is_none")]
	pub exit_reason: Option<String>,
}

/// The exit code of a run that failed before its setup phase, so before it submitted anything
/// to a chain: the same code as a failed `polkameter preflight`.
pub const PREFLIGHT_EXIT_CODE: i32 = 3;
/// How a run ended. The serialized names are part of `execution.json`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
	Completed,
	CompletedWithFailures,
	Stopped,
	Failed,
	/// Provisional: a stop was requested and the run is still tearing down. The final outcome
	/// replaces it; a run directory left in this state was killed before it could finish.
	Stopping,
}
/// How teardown ended. The serialized names are part of `execution.json`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Teardown {
	/// Every teardown step succeeded.
	Completed,
	/// Every teardown step ran, and at least one failed.
	Failed,
	/// A forced stop ended teardown before every step ran.
	Interrupted,
}
impl std::fmt::Display for RunState {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.write_str(match self {
			Self::Completed => "completed",
			Self::CompletedWithFailures => "completed_with_failures",
			Self::Stopped => "stopped",
			Self::Failed => "failed",
			Self::Stopping => "stopping",
		})
	}
}

/// Event writer shared by every task of a run. Events are redacted before they are persisted
/// and before the sink sees them.
#[derive(Clone)]
struct Log {
	file: Arc<Mutex<File>>,
	sink: EventSink,
	/// The run's credentials and endpoint secrets.
	redactor: Redactor,
}
impl Log {
	fn open(directory: &Path, sink: EventSink, redactor: Redactor) -> Result<Self> {
		let file = OpenOptions::new()
			.create_new(true)
			.write(true)
			.open(directory.join("events.jsonl"))?;
		Ok(Self { file: Arc::new(Mutex::new(file)), sink, redactor })
	}
	fn emit(&self, mut value: Value) -> Result<()> {
		value["timestamp"] = json!(polkameter_plugin_sdk::observers::now_ms());
		value["version"] = json!(2);
		self.redactor.value(&mut value);
		let text = serde_json::to_string(&value)?;
		let mut file = self.file.lock().map_err(|_| anyhow::anyhow!("event writer poisoned"))?;
		writeln!(file, "{text}")?;
		file.flush()?;
		(self.sink)(value);
		Ok(())
	}
	fn phase(&self, phase: &str) -> Result<()> {
		self.emit(json!({"event": "phase", "phase": phase}))
	}
}
/// The string values of the resolved credentials: each is redacted from events and errors.
fn secret_values(credentials: &Values) -> Vec<String> {
	credentials.values().filter_map(|v| v.as_str().map(str::to_owned)).collect()
}
/// A warning for each credential so short that redacting it may also hide text that is not it.
/// The credential is still used and redacted. A well-known development key such as `//Alice` is
/// public, so it gets no warning.
fn short_credentials(credentials: &Values) -> Vec<String> {
	credentials
		.iter()
		.filter(|(_, value)| {
			value.as_str().is_some_and(|v| crate::redact::is_short(v) && !crate::redact::is_dev_key(v))
		})
		.map(|(name, _)| {
			format!(
				"{name} is shorter than {} bytes: wherever its value appears in the run's files and logs it is redacted, also inside unrelated text",
				crate::redact::MIN_ENDPOINT_SECRET
			)
		})
		.collect()
}
/// The endpoints whose user information, query values and fragments are redacted: the plan's
/// targets and, when the topology can be read, its monitor URLs.
fn endpoints(plan: &Plan, registry: &Registry) -> Vec<String> {
	let mut endpoints: Vec<String> =
		plan.targets.entries.iter().map(|target| target.endpoint.clone()).collect();
	if let Ok(Some(targets)) = wiring::monitor_targets(plan, registry) {
		endpoints.extend(targets.into_iter().map(|target| target.url));
	}
	endpoints
}
/// The redactor of `plan` with the given credential values.
fn redactor(plan: &Plan, registry: &Registry, secrets: &[String]) -> Redactor {
	Redactor::new(secrets, endpoints(plan, registry))
}

/// Scratch directory removed when dropped, so early returns clean up too.
struct Scratch(PathBuf);
impl Drop for Scratch {
	fn drop(&mut self) {
		let _ = std::fs::remove_dir_all(&self.0);
	}
}

/// The contract of a built-in `core.` operation, or `None` for an unknown name.
fn core_contract(name: &str) -> Option<Operation> {
	Some(match name {
		"core.echo" => {
			Operation::new(name, &[("value", Schema::Json)], &[("value", Schema::Json)]).read_only()
		},
		"core.assert-equal" => Operation::new(
			name,
			&[("actual", Schema::Json), ("expected", Schema::Json)],
			&[("passed", Schema::Boolean)],
		)
		.read_only(),
		"core.submit-prepared" | "core.submit-setup" => Operation::new(
			name,
			&[("target", Schema::String), ("transactions", Schema::Json)],
			&[
				("hashes", Schema::Array { items: Box::new(Schema::String) }),
				("at", Schema::String),
			],
		),
		_ => return None,
	})
}

pub fn resolve(values: &Values, reference: &str) -> Result<Value> {
	if let Some(value) = values.get(reference) {
		return Ok(value.clone());
	}
	let (parent, key) = reference.rsplit_once('.').context("invalid reference")?;
	values
		.get(parent)
		.and_then(|v| v.get(key))
		.cloned()
		.with_context(|| format!("output {reference} is unavailable"))
}
/// The endpoint of a plan target, by its ID.
pub(crate) fn target_endpoint<'a>(plan: &'a Plan, id: &str) -> Result<&'a str> {
	plan.targets
		.entries
		.iter()
		.find(|target| target.id == id)
		.map(|target| target.endpoint.as_str())
		.with_context(|| format!("target {id} is not in the plan"))
}
pub fn transactions(value: &Value, directory: &Path) -> Result<Vec<PreparedTx>> {
	let txs: Vec<PreparedTx> = if value.is_array() {
		serde_json::from_value(value.clone())?
	} else {
		serde_json::from_value::<Artifact>(value.clone())?.read(directory)?
	};
	ensure!(txs.len() <= 100_000, "transaction source too large");
	for tx in &txs {
		tx.decode()?;
	}
	Ok(txs)
}
/// The built-in operations, in the order the plan reference lists them.
const CORE_OPERATIONS: [&str; 4] =
	["core.echo", "core.assert-equal", "core.submit-prepared", "core.submit-setup"];

fn contract<'a>(plugins: &'a Plugins, name: &str) -> Result<Cow<'a, Operation>> {
	lookup(Contracts::Plugins(plugins), name)?.context("invalid operation")
}

/// Where operation contracts come from: the started plugins, or only the built-in operations,
/// for `polkameter validate`, which starts no plugin.
#[derive(Clone, Copy)]
enum Contracts<'a> {
	Plugins(&'a Plugins),
	CoreOnly,
}

/// The contract of operation `name`; `None` for a plugin operation without plugins.
fn lookup<'a>(contracts: Contracts<'a>, name: &str) -> Result<Option<Cow<'a, Operation>>> {
	if name.starts_with("core.") {
		let operation = core_contract(name).with_context(|| {
			let near = crate::engine::plan::closest(name, CORE_OPERATIONS)
				.map_or_else(String::new, |near| format!("did you mean {near}? "));
			format!(
				"unknown built-in operation {name}; {near}the core operations are {}",
				CORE_OPERATIONS.join(", ")
			)
		})?;
		return Ok(Some(Cow::Owned(operation)));
	}
	let Contracts::Plugins(plugins) = contracts else { return Ok(None) };
	let (plugin, operation) = name.split_once('.').context("invalid operation")?;
	Ok(Some(Cow::Borrowed(plugins.operation(plugin, operation)?)))
}
/// Names a step in an error: `step `id` (plugin.op)`, located at the step's element.
fn step_error(step: &Step, message: impl std::fmt::Display) -> anyhow::Error {
	crate::engine::plan::PlanError::at(
		step.location,
		format!("step `{}` ({}): {message}", step.id, step.operation),
	)
}
/// The names in `names`, for an error message, or `none`.
fn listed<'a>(names: impl Iterator<Item = &'a String>) -> String {
	let names: Vec<_> = names.map(|name| format!("`{name}`")).collect();
	if names.is_empty() { "none".to_owned() } else { names.join(", ") }
}
/// The kind of a JSON value in words, for error messages.
fn value_kind(value: &Value) -> &'static str {
	match value {
		Value::Null => "null",
		Value::Bool(_) => "a boolean",
		Value::Number(n) if n.is_i64() || n.is_u64() => "an integer",
		Value::Number(_) => "a number",
		Value::String(_) => "a string",
		Value::Array(_) => "an array",
		Value::Object(_) => "an object",
	}
}
/// Checks the built-in operations of a plan without starting plugins: their names, inputs,
/// literals and the references between them. Steps of plugin operations are skipped, and their
/// outputs accepted as any value; `preflight` checks them against the manifests.
pub fn validate_core_contracts(plan: &Plan) -> Result<()> {
	check_contracts(plan, Contracts::CoreOnly)
}
/// Validate operation names, literals, references and output types before any mutation.
fn validate_contracts(plan: &Plan, plugins: &Plugins) -> Result<()> {
	check_contracts(plan, Contracts::Plugins(plugins))
}
fn check_contracts(plan: &Plan, contracts: Contracts<'_>) -> Result<()> {
	let mut types: BTreeMap<String, Schema> = plan
		.values("validate")
		.into_iter()
		.map(|(k, v)| (k, if v.is_string() { Schema::String } else { Schema::Integer }))
		.collect();
	types.extend(
		plan.credentials
			.entries
			.iter()
			.map(|c| (format!("credentials.{}", c.id), Schema::String)),
	);
	// Steps whose outputs are known, with their operation; and steps whose contract is unknown
	// here (plugin operations without plugins), whose outputs may be anything.
	let mut producers: BTreeMap<&str, (&str, Vec<String>)> = BTreeMap::new();
	let mut opaque: BTreeSet<&str> = BTreeSet::new();
	let step_of = |reference: &str| -> Option<String> {
		reference
			.strip_prefix("steps.")
			.and_then(|rest| rest.split('.').next())
			.map(str::to_owned)
	};
	for step in plan.all_steps() {
		let Some(operation) =
			lookup(contracts, &step.operation).map_err(|e| step_error(step, e))?
		else {
			opaque.insert(step.id.as_str());
			continue;
		};
		// An unknown name first: it is often a misspelt required input.
		if let Some(input) =
			step.inputs.iter().find(|input| !operation.inputs.contains_key(&input.name))
		{
			let near = crate::engine::plan::closest(
				&input.name,
				operation.inputs.keys().map(String::as_str),
			)
			.map_or_else(String::new, |near| format!("did you mean `{near}`? "));
			return Err(crate::engine::plan::PlanError::at(
				input.location,
				format!(
					"step `{}` ({}): input `{}` is not an input of {}; {near}its inputs are {}",
					step.id,
					step.operation,
					input.name,
					step.operation,
					listed(operation.inputs.keys())
				),
			));
		}
		let input_names =
			step.inputs.iter().map(|input| input.name.as_str()).collect::<BTreeSet<_>>();
		for (name, field) in &operation.inputs {
			if !field.optional && !input_names.contains(name.as_str()) {
				return Err(step_error(
					step,
					format!("missing required input `{name}`; add <input name=\"{name}\" …/>"),
				));
			}
		}
		for input in &step.inputs {
			let name = &input.name;
			let at = |message: String| {
				crate::engine::plan::PlanError::at(
					input.location,
					format!("step `{}` ({}): input `{name}` {message}", step.id, step.operation),
				)
			};
			// Every name is known: unknown names failed above.
			let expected = &operation.inputs[name];
			if let InputSource::Ref(reference) = &input.source {
				let producer = step_of(reference);
				if producer.as_deref().is_some_and(|id| opaque.contains(id)) {
					continue;
				}
				let Some(actual) = types.get(reference) else {
					// `steps.<id>.<output>` with a misspelt output; not `steps.<id>` alone.
					let known = producer
						.as_deref()
						.filter(|_| reference.matches('.').count() == 2)
						.and_then(|id| producers.get(id).map(|p| (id, p)));
					return Err(at(match known {
						Some((id, (operation, outputs))) => format!(
							"refers to `{reference}`; use one of step `{id}`'s outputs: {} \
							 ({operation} has no output `{}`)",
							listed(outputs.iter()),
							reference.rsplit('.').next().unwrap_or_default()
						),
						None => format!(
							"refers to `{reference}`, which is not an output of an earlier step; \
							 check the output names with `polkameter plugin inspect <plan>`"
						),
					}));
				};
				if !(actual == &expected.schema
					|| matches!(actual, Schema::Json)
					|| matches!(expected.schema, Schema::Json))
				{
					return Err(at(format!(
						"refers to `{reference}`, which is {}, but {} expects {}",
						schema_name(actual),
						step.operation,
						schema_name(&expected.schema)
					)));
				}
			} else {
				let literal = input.literal().map_err(|e| at(e.to_string()))?;
				if let Err(e) = expected.schema.validate(&literal) {
					let InputSource::Value(text) = &input.source else { unreachable!() };
					let expects = schema_name(&expected.schema);
					let scalar = !matches!(
						expected.schema,
						Schema::Array { .. } | Schema::Object { .. } | Schema::Json
					);
					return Err(at(if scalar {
						format!(
							"has value=\"{text}\", which is {}, but {} expects {expects}",
							value_kind(&literal),
							step.operation
						)
					} else {
						format!(
							"has value=\"{text}\", which {} does not accept ({e}); it expects \
							 {expects}",
							step.operation
						)
					}));
				}
			}
		}
		for (name, field) in &operation.outputs {
			types.insert(format!("steps.{}.{name}", step.id), field.schema.clone());
		}
		producers.insert(
			step.id.as_str(),
			(step.operation.as_str(), operation.outputs.keys().cloned().collect()),
		);
	}
	for step in &plan.preflight.entries {
		let Some(operation) = lookup(contracts, &step.operation)? else { continue };
		if !operation.read_only {
			return Err(step_error(
				step,
				"this operation can change chain state, so it cannot run in <preflight>; move \
				 the step to <setup>",
			));
		}
	}
	if let Some(load) = &plan.load {
		let at = |message: String| crate::engine::plan::PlanError::at(load.location, message);
		for (attribute, reference) in
			[("source-ref", &load.source), ("probes-ref", &load.probes_ref)]
		{
			let unknown = step_of(reference).is_some_and(|id| opaque.contains(id.as_str()));
			if !unknown && !types.contains_key(reference) {
				return Err(at(format!(
					"<load> {attribute} `{reference}` is not an output of a step before <load>; \
					 check the output names with `polkameter plugin inspect <plan>`"
				)));
			}
		}
		if let Some(state) = &load.state {
			lookup(contracts, &state.check).map_err(|e| at(format!("<load> state-check: {e}")))?;
		}
	}
	Ok(())
}
/// A schema in words, for error messages.
fn schema_name(schema: &Schema) -> String {
	match schema {
		Schema::Json => "any JSON value".into(),
		Schema::String => "a string".into(),
		Schema::Number => "a number".into(),
		Schema::Integer => "an integer".into(),
		Schema::Boolean => "a boolean".into(),
		Schema::Array { items } => format!("an array of {}", schema_name(items)),
		Schema::Object { fields } => format!("an object with fields {}", listed(fields.keys())),
	}
}
fn next_directory_sequence() -> u64 {
	static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
	NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// Starts plugins in a fresh scratch directory for host work that never arms setup or load.
/// Plugins are shut down and the directory removed on every return path, including early errors.
/// A panic or a dropped future skips the graceful shutdown; the plugin processes are then killed
/// on drop instead.
async fn with_scratch_plugins<T>(
	plan: &Plan,
	registry: &Registry,
	purpose: &str,
	body: impl AsyncFnOnce(&Plugins, &Context, &Path) -> Result<T>,
) -> Result<T> {
	let directory = std::env::temp_dir().join(format!(
		"polkameter-{purpose}-{}-{}-{}",
		std::process::id(),
		polkameter_plugin_sdk::observers::now_ms(),
		next_directory_sequence()
	));
	std::fs::create_dir(&directory)?;
	let _scratch = Scratch(directory.clone());
	let context = Context::host(purpose, &directory);
	let plugins = Plugins::start(&plan.plugins.entries, registry, &context, None).await?;
	// Boxed: the bodies are large and would otherwise sit on the caller's stack.
	let result = Box::pin(body(&plugins, &context, &directory)).await;
	plugins.shutdown(&context, &CancellationToken::new()).await;
	result
}
pub async fn inspect(plan: &Plan, registry: &Registry) -> Result<Value> {
	let mut inspected = inspect_plugins(plan, registry).await?;
	// The targets' endpoints may hold API keys.
	redactor(plan, registry, &[]).value(&mut inspected["targets"]);
	Ok(inspected)
}
async fn inspect_plugins(plan: &Plan, registry: &Registry) -> Result<Value> {
	with_scratch_plugins(plan, registry, "inspect", async |plugins, _, _| {
		validate_contracts(plan, plugins)?;
		Ok(json!({"valid": true, "plugins": plugins.manifests, "targets": plan.targets}))
	})
	.await
}
/// Checks the plan against its plugins, credentials and chain, and runs its `<preflight>` steps
/// and the calibration. Never arms setup or load and writes only to its scratch directory.
pub async fn preflight(plan: &Plan, registry: &Registry) -> Result<()> {
	with_scratch_plugins(plan, registry, "preflight", async |plugins, context, directory| {
		validate_contracts(plan, plugins)?;
		// Every credential the plan declares must resolve, as a run needs it; no value is used.
		registry.check_credentials(plan)?;
		check_environment(plan, registry).await?;
		let mut values = plan.values("preflight");
		let mut redactor = redactor(plan, registry, &[]);
		let mut _diagnostics = crate::redact::activate(&redactor);
		// Resolve credentials only if an explicit preflight step references one.
		if plan
			.preflight
			.entries
			.iter()
			.flat_map(|s| &s.inputs)
			.any(|i| matches!(&i.source, InputSource::Ref(r) if r.starts_with("credentials.")))
		{
			let credentials = registry.resolve_credentials(plan)?;
			redactor = self::redactor(plan, registry, &secret_values(&credentials));
			_diagnostics = crate::redact::activate(&redactor);
			for warning in short_credentials(&credentials) {
				crate::redact::diag!("{} warning: {warning}", crate::PRODUCT_NAME);
			}
			values.extend(credentials);
		}
		let redacted = |e: anyhow::Error| anyhow::anyhow!(redactor.text(&format!("{e:#}")));
		for step in &plan.preflight.entries {
			// Plugin errors may echo a credential, so they are redacted as the run's are.
			let output =
				invoke(step, plugins, &values, context, &CancellationToken::new(), &redactor)
					.await
					.map_err(redacted)?;
			values.insert(format!("steps.{}", step.id), output);
		}
		calibrate(plan, directory).await.map_err(redacted)
	})
	.await
	.map_err(|e| {
		// Plan errors keep their position. Monitor and RPC checks may report an endpoint.
		if e.downcast_ref::<crate::engine::plan::PlanError>().is_some() {
			return e;
		}
		anyhow::anyhow!(redactor(plan, registry, &[]).text(&format!("{e:#}")))
	})
}
async fn check_environment(plan: &Plan, registry: &Registry) -> Result<()> {
	let (Some(monitor), Some(targets)) = (&plan.monitors, wiring::monitor_targets(plan, registry)?)
	else {
		return Ok(());
	};
	for warning in crate::engine::monitors::preflight::preflight(&targets).await? {
		crate::redact::diag!("preflight: {warning}");
	}
	let metrics = monitor
		.metrics
		.iter()
		.map(|m| Ok((m.role.parse::<Job>()?, m.name.clone())))
		.collect::<Result<Vec<_>>>()?;
	crate::engine::monitors::preflight::require_metrics(&targets, &metrics).await?;
	Ok(())
}
/// Calibrate during preflight, before any recognition, signing or proving in setup.
async fn calibrate(plan: &Plan, directory: &Path) -> Result<()> {
	let Some(load) = &plan.load else { return Ok(()) };
	let endpoint = target_endpoint(plan, &load.target)?;
	let measured = async {
		let client = Client::connect(endpoint).await?;
		block_interval_s(&client).await
	}
	.await
	.context("block interval calibration failed during preflight")?;
	let calibration = json!({
		"version": 1,
		"configuredBlockIntervalSeconds": load.block_interval_seconds,
		"measuredBlockIntervalSeconds": measured,
		"relativeTolerance": 0.25
	});
	// Preserve the evidence even when the mismatch prevents setup.
	RunDir::open(directory).write_json("calibration.json", &calibration)?;
	validate_interval(load.block_interval_seconds, measured)
}

/// The chain's block interval: the `Timestamp.Now` span of the last 60 blocks since the fork
/// started, over their count. Not a median or trimmed mean of gaps: on a fork the timestamps
/// come in bunches (0 s / 12 s pairs on 1 core, 12 s then five 0 s on 3 cores), and only the
/// span is right. A fork just started has fewer blocks: this waits for 20 of them.
pub(crate) async fn block_interval_s(client: &Client) -> Result<f64, ChainError> {
	let deadline = tokio::time::Instant::now() + INTERVAL_WAIT;
	loop {
		let number = client.best_number().await?;
		let all = timestamps(client, number).await?;
		let stamps = since_restart(&all);
		if let Some(s) = interval_s(stamps) {
			return Ok(s);
		}
		if tokio::time::Instant::now() >= deadline {
			return Err(ChainError::Timeout(format!(
				"block interval: {} blocks since the fork started at block {number}, {MIN_INTERVALS} needed, after {INTERVAL_WAIT:?}",
				stamps.len().saturating_sub(1)
			)));
		}
		tokio::time::sleep(std::time::Duration::from_secs(6)).await;
	}
}

/// `Timestamp.Now` of block `number` and up to 60 before it, newest first; the walk stops
/// at a block the node doesn't have (a fork keeps none from before its bite).
async fn timestamps(client: &Client, number: u32) -> Result<Vec<u64>, ChainError> {
	let mut stamps = Vec::new();
	for n in (number.saturating_sub(60)..=number).rev() {
		let hash: Option<subxt::utils::H256> =
			client.request("chain_getBlockHash", subxt_rpcs::rpc_params![n]).await?;
		let Some(hash) = hash else { break };
		stamps.push(
			fetch::<(), u64>(&client.at(hash.0).await?, "Timestamp", "Now", ())
				.await?
				.unwrap_or(0),
		);
	}
	Ok(stamps)
}

/// A gap between two blocks longer than any slot: the bite to the fork's first block (or a
/// stall). On a fork the longest normal gap is 12 s.
const RESTART_GAP_MS: u64 = 60_000;
/// Blocks the interval spans at least: enough whole timestamp bunches (six blocks on 3 cores)
/// for the span to be right.
const MIN_INTERVALS: usize = 20;
/// How long to wait for `MIN_INTERVALS` blocks after a fork started.
const INTERVAL_WAIT: std::time::Duration = std::time::Duration::from_secs(120);

/// The newest blocks of `stamps` (newest first, ms) up to the first gap over `RESTART_GAP_MS`:
/// the blocks since the fork started.
fn since_restart(stamps: &[u64]) -> &[u64] {
	let first_gap = stamps.windows(2).position(|w| w[0].saturating_sub(w[1]) > RESTART_GAP_MS);
	&stamps[..first_gap.map_or(stamps.len(), |i| i + 1)]
}

/// The block interval, s, over `stamps` (newest first, ms); `None` with fewer than
/// `MIN_INTERVALS` blocks or a zero span.
fn interval_s(stamps: &[u64]) -> Option<f64> {
	let (newest, oldest) = (stamps.first()?, stamps.last()?);
	let intervals = stamps.len() - 1;
	let span = newest.saturating_sub(*oldest);
	(intervals >= MIN_INTERVALS && span > 0).then(|| span as f64 / intervals as f64 / 1000.0)
}

/// How a run exits.
#[derive(Clone, Copy, Debug, Default)]
pub struct RunOptions {
	pub fail_on: FailOn,
}

/// Which load-test verdicts make a run exit 1, besides runtime and smoke-mode failures.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, clap::ValueEnum)]
pub enum FailOn {
	/// Only a failed run.
	Error,
	/// Also a FAIL verdict.
	Verdict,
	/// Also a FAIL or INCONCLUSIVE verdict.
	#[default]
	VerdictOrInconclusive,
}

impl FailOn {
	/// The `--fail-on` value.
	pub fn name(self) -> &'static str {
		match self {
			FailOn::Error => "error",
			FailOn::Verdict => "verdict",
			FailOn::VerdictOrInconclusive => "verdict-or-inconclusive",
		}
	}
}

/// Exit code of a run stopped by `Stop::request` without an explicit code (SIGINT, `/stop`).
pub const STOPPED_EXIT_CODE: i32 = 130;
/// The teardown error of a run whose stop was forced.
const TEARDOWN_INTERRUPTED: &str = "teardown was interrupted by a forced stop";

/// How a run is asked to stop, in two stages.
///
/// The first request ends the running phases and starts teardown, which still runs every step
/// under its own `timeout-ms`. Forcing also ends teardown: in-flight plugin calls are abandoned,
/// their processes are killed and reaped, and the outcome is written straight away.
#[derive(Clone, Debug)]
pub struct Stop {
	cancel: CancellationToken,
	force: CancellationToken,
	exit_code: Arc<std::sync::atomic::AtomicI32>,
}
impl Default for Stop {
	fn default() -> Self {
		Self {
			cancel: CancellationToken::new(),
			force: CancellationToken::new(),
			exit_code: Arc::new(STOPPED_EXIT_CODE.into()),
		}
	}
}
impl Stop {
	/// Ends the running phases; teardown still runs. The first request's `exit_code` is kept
	/// as the stopped run's exit code.
	pub fn request(&self, exit_code: i32) {
		if !self.cancel.is_cancelled() {
			self.exit_code.store(exit_code, std::sync::atomic::Ordering::SeqCst);
		}
		self.cancel.cancel();
	}
	/// Ends the run and its teardown now.
	pub fn force(&self, exit_code: i32) {
		self.request(exit_code);
		self.force.cancel();
	}
	fn exit_code(&self) -> i32 {
		self.exit_code.load(std::sync::atomic::Ordering::SeqCst)
	}
}

/// Runs a plan in a new directory under `root`. Phases: preflight, setup, measurement,
/// evaluate, then teardown, which runs whenever the plugins started. The outcome is written to the
/// run directory and returned. The run can be stopped in two stages; see [`Stop`]. A stopped
/// run's exit code is the one passed to the first [`Stop::request`].
pub async fn run_stoppable(
	plan: Plan,
	registry: Registry,
	root: &Path,
	stop: Stop,
	sink: EventSink,
	options: RunOptions,
) -> Result<Outcome> {
	// A new `run-…` directory under `root`: a run never reuses one.
	std::fs::create_dir_all(root)?;
	let run_id = format!(
		"run-{}-{}-{}",
		polkameter_plugin_sdk::observers::now_ms(),
		std::process::id(),
		next_directory_sequence()
	);
	let directory = root.join(&run_id);
	std::fs::create_dir(&directory).context("run directory must be new")?;
	let credentials = registry.resolve_credentials(&plan);
	let secrets = credentials.as_ref().map(secret_values).unwrap_or_default();
	let redactor = redactor(&plan, &registry, &secrets);
	// What the run prints on standard error is redacted too, until it ends.
	let _diagnostics = crate::redact::activate(&redactor);
	let log = Log::open(&directory, sink, redactor)?;
	if let Ok(credentials) = &credentials {
		for warning in short_credentials(credentials) {
			crate::redact::diag!("{} warning: {warning}", crate::PRODUCT_NAME);
			log.emit(json!({"event": "warning", "message": warning}))?;
		}
	}
	let recorded_plan = log.redactor.json(&plan)?;
	RunDir::open(&directory).write_json("plan.json", &recorded_plan)?;
	let context = Context::host(run_id, directory.canonicalize()?);
	let run = Run {
		plan: &plan,
		registry: &registry,
		context,
		directory,
		cancel: stop.cancel.clone(),
		stop,
		log,
		started: Instant::now(),
		options,
		teardown: Mutex::new(None),
		phase: Mutex::new(PREFLIGHT.to_owned()),
		failed_in: Mutex::new(None),
		exit_reason: Mutex::new(None),
	};
	let execute = Box::pin(run.execute(credentials));
	let mut execute = std::pin::pin!(execute);
	let result = tokio::select! {
		biased;
		result = &mut execute => result,
		() = run.cancel.cancelled() => {
			run.stopping();
			execute.await
		},
	};
	run.outcome(result)
}

/// The phase of a run before setup: credential, plugin, contract and environment checks,
/// `<preflight>` steps and calibration.
const PREFLIGHT: &str = "preflight";

/// One run in progress: its plan and host, its directory and event log, and its deadline.
struct Run<'a> {
	plan: &'a Plan,
	registry: &'a Registry,
	context: Context,
	directory: PathBuf,
	cancel: CancellationToken,
	stop: Stop,
	log: Log,
	started: Instant,
	options: RunOptions,
	teardown: Mutex<Option<Teardown>>,
	/// The phase the run is in; `preflight` until setup starts.
	phase: Mutex<String>,
	/// The phase the run's phases failed in, kept before teardown enters its own phase.
	failed_in: Mutex<Option<String>>,
	/// Why the report's exit code is 1, kept for the outcome.
	exit_reason: Mutex<Option<String>>,
}

impl Run<'_> {
	/// As soon as a stop is requested: a provisional `execution.json`, so a run killed during
	/// teardown still says it was stopping, and plugin logs redacted early. Best effort.
	fn stopping(&self) {
		let code = self.stop.exit_code();
		let reason = match code {
			130 => "SIGINT or a stop request",
			143 => "SIGTERM",
			_ => "a stop request",
		};
		let provisional = json!({
			"artifact_dir": self.context.artifact_dir.to_string_lossy(),
			"exit_code": code,
			"state": RunState::Stopping,
			"error": format!("stopped by {reason}; teardown was still running"),
			"stop_requested_at": polkameter_plugin_sdk::observers::now_ms(),
		});
		let _ = RunDir::open(&self.directory).write_json("execution.json", &provisional);
		let _ = redact_plugin_logs(&self.directory, &self.log.redactor);
	}

	/// Logs and records the phase the run enters.
	fn enter(&self, phase: &str) -> Result<()> {
		if let Ok(mut current) = self.phase.lock() {
			phase.clone_into(&mut current);
		}
		self.log.phase(phase)
	}

	/// Whether the run is still in its preflight checks: setup has not started, so nothing was
	/// submitted to a chain.
	fn before_setup(&self) -> bool {
		self.phase.lock().is_ok_and(|phase| *phase == PREFLIGHT)
	}

	/// Checks the credentials, starts the plugins, runs the phases under the whole-run deadline,
	/// then tears down. A run that fails before its setup phase submitted nothing, so it skips
	/// teardown.
	async fn execute(&self, credentials: Result<Values>) -> Result<i32> {
		self.enter(PREFLIGHT)?;
		let credentials = credentials?;
		let plugins = self.start_plugins().await?;
		let mut values = self.values(credentials);
		let result = self.bounded(Box::pin(self.phases(&plugins, &mut values))).await;
		if result.is_err() && self.before_setup() {
			plugins.shutdown(&self.context, &self.stop.force).await;
			return result;
		}
		if result.is_err()
			&& let (Ok(phase), Ok(mut failed_in)) = (self.phase.lock(), self.failed_in.lock())
		{
			*failed_in = Some(phase.clone());
		}
		let cleanup = self.teardown(&plugins, &mut values).await;
		plugins.shutdown(&self.context, &self.stop.force).await;
		match (result, cleanup) {
			(Ok(code), Ok(())) => Ok(code),
			(Err(error), Ok(())) | (Ok(_), Err(error)) => Err(error),
			// Both failed: the run's error comes first, and the teardown's is kept with it.
			(Err(error), Err(cleanup)) => {
				Err(anyhow::anyhow!("{error}; teardown failed: {cleanup}"))
			},
		}
	}

	async fn start_plugins(&self) -> Result<Plugins> {
		cancellable(&self.cancel, async {
			tokio::time::timeout(
				Duration::from_millis(self.plan.timeout_ms),
				Plugins::start(
					&self.plan.plugins.entries,
					self.registry,
					&self.context,
					Some(&self.log.redactor),
				),
			)
			.await
			.context("whole-run deadline exceeded during plugin startup")?
		})
		.await
	}

	fn values(&self, credentials: Values) -> Values {
		let mut values = self.plan.values(&self.context.run_id);
		values.extend(credentials);
		// Lossy, not a panic, for a directory path that is not UTF-8 (the CLI refuses those).
		values.insert("run.directory".into(), json!(self.context.artifact_dir.to_string_lossy()));
		values
	}

	/// Runs `body` under the whole-run deadline and the cancellation token.
	async fn bounded(&self, body: impl Future<Output = Result<i32>>) -> Result<i32> {
		let budget =
			Duration::from_millis(self.plan.timeout_ms).saturating_sub(self.started.elapsed());
		cancellable(&self.cancel, async {
			tokio::time::timeout(budget, body)
				.await
				.unwrap_or_else(|_| Err(anyhow::anyhow!("whole-run deadline exceeded")))
		})
		.await
	}

	/// The phases of a run, in order. Returns the exit code, which the measurement's report sets.
	async fn phases(&self, plugins: &Plugins, values: &mut Values) -> Result<i32> {
		validate_contracts(self.plan, plugins)?;
		self.write_resolved(plugins)?;
		check_environment(self.plan, self.registry).await?;
		self.run_steps(&self.plan.preflight.entries, plugins, values).await?;
		calibrate(self.plan, &self.directory).await?;
		self.enter("setup")?;
		self.run_steps(&self.plan.setup.entries, plugins, values).await?;
		let measured = match &self.plan.load {
			Some(_) => {
				self.enter("measurement")?;
				let setup_seconds = self.started.elapsed().as_secs();
				let progress = |phase: &str| self.enter(phase);
				let measured = measurement::run(
					self.plan,
					plugins,
					self.registry,
					values,
					&self.context,
					measurement::Reporting {
						setup_seconds,
						progress: &progress,
						redactor: &self.log.redactor,
					},
				)
				.await?;
				measured
					.write_summary(&RunDir::open(&self.context.artifact_dir), &self.log.redactor)?;
				Some(measured)
			},
			None => None,
		};
		for warning in measured.iter().flat_map(measurement::Measured::warnings) {
			self.log.emit(json!({"event": "warning", "message": warning}))?;
		}
		let evaluated = self.run_steps(&self.plan.evaluate.entries, plugins, values).await;
		// The report keeps the built-in evidence even when a plugin check is malformed; that error
		// is the run's, after the report is written.
		let mut checks_error = None;
		let code = match measured {
			Some(measured) => {
				self.enter("checks")?;
				let dir = RunDir::open(&self.context.artifact_dir);
				let checks = plugin_checks(self.plan, plugins, values).unwrap_or_else(|e| {
					checks_error = Some(e);
					Vec::new()
				});
				let exit =
					measurement::report(&dir, measured, &checks, self.options, &self.log.redactor)?;
				*self.exit_reason.lock().unwrap_or_else(|e| e.into_inner()) = exit.reason;
				exit.code
			},
			None => 0,
		};
		evaluated?;
		checks_error.map_or(Ok(code), Err)
	}

	/// The resolved plan and, with monitors, the resolved topology: what this host ran with.
	fn write_resolved(&self, plugins: &Plugins) -> Result<()> {
		let resolved = json!({
			"plugins": plugins.manifests,
			"topologies": self.plan.monitors,
			"installations": self.registry.plugins.iter()
				.filter(|(id, _)| plugins.manifests.contains_key(*id))
				.collect::<BTreeMap<_, _>>(),
			"host": {
				"engineVersion": env!("CARGO_PKG_VERSION"),
				"blake2": checksum(&std::env::current_exe()?)?,
			},
		});
		RunDir::open(&self.directory)
			.write_json("resolved-plan.json", &self.log.redactor.json(&resolved)?)?;
		if let (Some(monitors), Some(targets)) =
			(&self.plan.monitors, wiring::monitor_targets(self.plan, self.registry)?)
		{
			let topology = self
				.registry
				.topologies
				.get(&monitors.topology)
				.context("topology is not installed")?;
			let target_records: Vec<_> = targets
				.iter()
				.map(|t| {
					json!({
						"role": t.job.label(),
						"instance": t.instance,
						"url": crate::redact::endpoint(&t.url),
					})
				})
				.collect();
			let resolved = json!({
				"topologyHash": checksum(topology)?,
				"targets": target_records,
			});
			RunDir::open(&self.directory)
				.write_json("resolved-targets.json", &self.log.redactor.json(&resolved)?)?;
		}
		Ok(())
	}

	async fn run_steps(
		&self,
		steps: &[Step],
		plugins: &Plugins,
		values: &mut Values,
	) -> Result<()> {
		execute_steps(steps, plugins, values, &self.context, &self.cancel, &self.log).await
	}

	/// Teardown runs even after a failure or cancellation, and runs every step even after one
	/// fails. Each step runs under its own `timeout-ms`; only a forced stop ends teardown early.
	/// Before each step, a plugin that a cancellation, crash or timeout ended is started again,
	/// so its teardown steps can still clean up.
	async fn teardown(&self, plugins: &Plugins, values: &mut Values) -> Result<()> {
		self.enter("teardown")?;
		let force = &self.stop.force;
		let mut errors = Vec::new();
		let mut state = Teardown::Completed;
		for step in &self.plan.teardown.entries {
			if force.is_cancelled() {
				state = Teardown::Interrupted;
				break;
			}
			self.restart_for(step, plugins).await?;
			let result = execute_steps(
				std::slice::from_ref(step),
				plugins,
				values,
				&self.context,
				force,
				&self.log,
			)
			.await;
			if let Err(error) = result {
				if force.is_cancelled() {
					state = Teardown::Interrupted;
					break;
				}
				state = Teardown::Failed;
				errors.push(error.to_string());
			}
		}
		if state == Teardown::Interrupted {
			errors.push(TEARDOWN_INTERRUPTED.into());
		}
		*self.teardown.lock().map_err(|_| anyhow::anyhow!("teardown state poisoned"))? =
			Some(state);
		if errors.is_empty() {
			return Ok(());
		}
		let error = errors.join("; ");
		self.log
			.emit(json!({"event": "cleanup-failed", "error": error, "errors": errors}))?;
		Err(anyhow::anyhow!(error))
	}

	/// Starts the plugin of a teardown step again if its process failed. A plugin that cannot be
	/// restarted is reported as a warning, and the step then fails on its own.
	async fn restart_for(&self, step: &Step, plugins: &Plugins) -> Result<()> {
		let Some((plugin, _)) = step.operation.split_once('.').filter(|(id, _)| *id != "core")
		else {
			return Ok(());
		};
		let restarted = tokio::select! {
			biased;
			() = self.stop.force.cancelled() => return Ok(()),
			restarted = plugins.restart_failed(plugin, self.registry, &self.context) => restarted,
		};
		match restarted {
			Ok(true) => self.log.emit(json!({"event": "plugin-restarted", "plugin": plugin})),
			Ok(false) => Ok(()),
			Err(error) => self.log.emit(json!({
				"event": "warning",
				"message": format!("plugin {plugin} could not be restarted for teardown: {error:#}"),
			})),
		}
	}

	/// Settles how the run ended, writes its artifacts and outcome, and logs the outcome.
	fn outcome(&self, result: Result<i32>) -> Result<Outcome> {
		let redacted = redact_plugin_logs(&self.directory, &self.log.redactor);
		let (exit_code, state, error) = match result {
			// A stop is honoured even when it came after the phases, during teardown.
			Ok(_) if self.cancel.is_cancelled() => (
				self.stop.exit_code(),
				RunState::Stopped,
				Some(anyhow::anyhow!("run stopped during teardown")),
			),
			Ok(0) => (0, RunState::Completed, None),
			Ok(code) => (code, RunState::CompletedWithFailures, None),
			Err(error) if self.cancel.is_cancelled() => {
				(self.stop.exit_code(), RunState::Stopped, Some(error))
			},
			Err(error) if self.before_setup() => {
				(PREFLIGHT_EXIT_CODE, RunState::Failed, Some(error))
			},
			Err(error) => (1, RunState::Failed, Some(error)),
		};
		// A run whose phases failed keeps that phase; otherwise it failed where it is now, such
		// as in teardown.
		let failed_phase = (state == RunState::Failed)
			.then(|| {
				self.failed_in
					.lock()
					.ok()
					.and_then(|failed_in| failed_in.clone())
					.or_else(|| self.phase.lock().ok().map(|phase| phase.clone()))
			})
			.flatten();
		let error_location = error
			.as_ref()
			.and_then(|error| error.downcast_ref::<crate::engine::plan::PlanError>())
			.and_then(|error| error.location);
		let mut outcome = Outcome {
			artifact_dir: self.context.artifact_dir.clone(),
			exit_code,
			state,
			error: error.map(|error| self.log.redactor.text(&error.to_string())),
			teardown: self.teardown.lock().ok().and_then(|state| *state),
			failed_phase,
			error_location,
			// Only a run the report failed has its reason: a later error or stop decides instead.
			exit_reason: (state == RunState::CompletedWithFailures)
				.then(|| self.exit_reason.lock().ok().and_then(|reason| reason.clone()))
				.flatten()
				.map(|reason| self.log.redactor.text(&reason)),
		};
		// Samples come from the raw files.
		for error in [redacted, artifacts::write_samples(&self.directory)]
			.into_iter()
			.filter_map(Result::err)
		{
			fail_report(&mut outcome, error);
		}
		RunDir::open(&self.directory).write_json("execution.json", &outcome)?;
		let summary = self.directory.join("summary.md");
		if summary.exists() {
			// The report redacted it as it was written; this is a second pass.
			let text = std::fs::read(&summary)?;
			std::fs::write(&summary, self.log.redactor.bytes(&text))?;
		} else {
			// The plan's name may hold a secret too; the whole text is redacted.
			let text = format!(
				"# {}\n\nStatus: {}\n\n{}\n",
				self.plan.name,
				outcome.state,
				outcome.error.as_deref().unwrap_or("All configured steps completed.")
			);
			std::fs::write(summary, self.log.redactor.text(&text))?;
		}
		// These files are also inputs to offline JUnit regeneration. They were redacted as they
		// were written; this second pass keeps a later report free of credentials whatever
		// wrote them.
		for name in ["summary.json", "plugin-checks.json"] {
			let path = self.directory.join(name);
			if path.exists() {
				let mut value: Value = serde_json::from_slice(&std::fs::read(&path)?)?;
				self.log.redactor.value(&mut value);
				RunDir::open(&self.directory).write_json(name, &value)?;
			}
		}
		if let Err(error) = crate::engine::junit::write(&self.directory, &self.log.redactor) {
			let event = crate::engine::junit::warning(None, &error.to_string(), &self.log.redactor);
			let _ = self.log.emit(event);
		}
		self.log.emit(json!({"event": "completed", "outcome": outcome}))?;
		Ok(outcome)
	}
}

/// Redacts each plugin's `stderr.log` again. PolkaMeter redacts plugin standard error as it
/// streams into the log; this pass, on the first stop request and when the run ends, also covers
/// anything that reached the file another way, such as a plugin writing to it by path. Each log
/// is read in bounded chunks; the log write lock is held only for what was appended meanwhile
/// and the replacement, so no streamed line is lost and streamed writes wait briefly.
fn redact_plugin_logs(directory: &Path, redactor: &Redactor) -> Result<()> {
	if redactor.is_empty() {
		return Ok(());
	}
	let Ok(plugins) = std::fs::read_dir(directory.join("plugins")) else { return Ok(()) };
	for plugin in plugins {
		let log = plugin?.path().join("stderr.log");
		match crate::redact::rewrite_file(&log, redactor, crate::engine::plugins::lock_logs) {
			Err(error) if error.kind() == std::io::ErrorKind::NotFound => {},
			result => result?,
		}
	}
	Ok(())
}

/// A report that cannot be written fails the run. A cancelled run keeps its state, and the error
/// is appended to the run's error.
fn fail_report(outcome: &mut Outcome, error: impl std::fmt::Display) {
	let report = format!("report generation failed: {error}");
	// A run that already failed keeps its exit code (3 before setup, else 1).
	if !matches!(outcome.state, RunState::Stopped | RunState::Failed) {
		outcome.exit_code = 1;
		outcome.state = RunState::Failed;
		outcome.exit_reason = None;
		outcome.failed_phase = Some("report".into());
	}
	outcome.error = Some(match outcome.error.take() {
		Some(error) => format!("{error}; {report}"),
		None => report,
	});
}

/// Runs the steps in order. Each step is logged when it starts and finishes; a failed step
/// stops the list.
async fn execute_steps(
	steps: &[Step],
	plugins: &Plugins,
	values: &mut Values,
	context: &Context,
	cancel: &CancellationToken,
	log: &Log,
) -> Result<()> {
	for step in steps {
		ensure!(!cancel.is_cancelled(), "run cancelled");
		let started = Instant::now();
		let mut event = step_event("step-started", step);
		event["operation"] = json!(step.operation);
		log.emit(event)?;
		let result: Result<Value> = with_progress(
			step,
			started,
			log,
			invoke(step, plugins, values, context, cancel, &log.redactor),
		)
		.await?;
		let mut event = step_event("step-finished", step);
		event["elapsedMs"] = json!(started.elapsed().as_millis());
		event["success"] = json!(result.is_ok());
		event["error"] = json!(result.as_ref().err().map(ToString::to_string));
		log.emit(event)?;
		values.insert(format!("steps.{}", step.id), result?);
	}
	Ok(())
}

fn step_event(kind: &str, step: &Step) -> Value {
	json!({"event": kind, "step": step.id})
}

/// Awaits a step, logging a `step-progress` event every five seconds until it completes.
async fn with_progress<T>(
	step: &Step,
	started: Instant,
	log: &Log,
	work: impl Future<Output = T>,
) -> Result<T> {
	let mut work = Box::pin(work);
	let mut progress = tokio::time::interval(Duration::from_secs(5));
	progress.tick().await;
	loop {
		tokio::select! {
			result = &mut work => return Ok(result),
			_ = progress.tick() => log.emit(json!({
				"event": "step-progress",
				"step": step.id,
				"elapsedMs": started.elapsed().as_millis(),
			}))?,
		}
	}
}

async fn invoke(
	step: &Step,
	plugins: &Plugins,
	values: &Values,
	context: &Context,
	cancel: &CancellationToken,
	redactor: &Redactor,
) -> Result<Value> {
	let inputs = Value::Object(
		step.inputs
			.iter()
			.map(|input| {
				let value = match &input.source {
					InputSource::Ref(reference) => resolve(values, reference),
					_ => input.literal(),
				}
				.map_err(|e| {
					anyhow::anyhow!(
						"step `{}` ({}): input `{}`: {e:#}",
						step.id,
						step.operation,
						input.name
					)
				})?;
				Ok((input.name.clone(), value))
			})
			.collect::<Result<serde_json::Map<_, _>>>()?,
	);
	let operation = contract(plugins, &step.operation)?;
	operation.validate_inputs(&inputs).map_err(|e| {
		anyhow::anyhow!(
			"step `{}` ({}): the inputs do not match the operation: {e:#}",
			step.id,
			step.operation
		)
	})?;
	if !step.operation.starts_with("core.") {
		return plugins.invoke(&step.operation, inputs, context, step.timeout_ms, cancel).await;
	}
	// Redact complete values before bounding them: a credential must never be truncated
	// into a prefix that the event/outcome redactor can no longer recognize.
	let work = Box::pin(core_operation(step, &inputs, context, redactor));
	cancellable(cancel, async {
		tokio::time::timeout(Duration::from_millis(step.timeout_ms), work)
			.await
			.map_err(|_| {
				anyhow::anyhow!("step {} timed out; submission outcome may be unknown", step.id)
			})?
	})
	.await
}

/// Maximum rendered characters, including JSON quotes, escapes and any ellipsis.
const ASSERTION_VALUE_CHARS: usize = 80;

fn assertion_value(value: &Value, redactor: &Redactor) -> String {
	let mut value = value.clone();
	redactor.value(&mut value);
	// JSON escapes ASCII controls; also escape Unicode controls that terminals can interpret.
	let rendered: String = value
		.to_string()
		.chars()
		.map(|c| {
			if crate::redact::is_unsafe_char(c) {
				format!("\\u{:04x}", c as u32)
			} else {
				c.to_string()
			}
		})
		.collect();
	if rendered.chars().count() <= ASSERTION_VALUE_CHARS {
		return rendered;
	}
	format!("{}…", crate::redact::truncate(&rendered, ASSERTION_VALUE_CHARS - 1))
}

fn assertion_text(safe: &str) -> String {
	let mut result: String = safe
		.chars()
		.take(ASSERTION_VALUE_CHARS)
		.map(|c| if crate::redact::is_unsafe_char(c) { ' ' } else { c })
		.collect();
	if safe.chars().count() > ASSERTION_VALUE_CHARS {
		result.push('…');
	}
	result
}

/// The built-in operations, which run in the host rather than in a plugin.
async fn core_operation(
	step: &Step,
	inputs: &Value,
	context: &Context,
	redactor: &Redactor,
) -> Result<Value> {
	match step.operation.as_str() {
		"core.echo" => Ok(json!({"value": inputs["value"]})),
		"core.assert-equal" => {
			ensure!(
				inputs["actual"] == inputs["expected"],
				"assertion {} failed: expected {}, got {}",
				assertion_text(&redactor.text(&step.id)),
				assertion_value(&inputs["expected"], redactor),
				assertion_value(&inputs["actual"], redactor)
			);
			Ok(json!({"passed": true}))
		},
		"core.submit-prepared" => submit(inputs, context, false).await,
		"core.submit-setup" => submit(inputs, context, true).await,
		other => bail!("unknown built-in operation {other}"),
	}
}

/// Resolves `work` unless the run is cancelled first.
async fn cancellable<T>(
	cancel: &CancellationToken,
	work: impl Future<Output = Result<T>>,
) -> Result<T> {
	tokio::select! {
		biased;
		_ = cancel.cancelled() => bail!("run cancelled"),
		result = work => result,
	}
}

async fn submit(inputs: &Value, context: &Context, wait_finalized: bool) -> Result<Value> {
	let client = polkameter_plugin_sdk::chain::Client::connect(
		inputs["target"].as_str().context("target missing")?,
	)
	.await?;
	let txs = transactions(&inputs["transactions"], &context.artifact_dir)?;
	let mut blocks = client.api().stream_blocks().await?;
	let mut hashes = Vec::new();
	let mut finalized_at = String::new();
	for tx in txs {
		let bytes = tx.decode()?;
		client.validate(&bytes, "prepared setup").await?;
		let hash = client.submit(&bytes).await?;
		ensure!(strip_0x(&hash) == strip_0x(&tx.hash), "RPC returned unexpected transaction hash");
		hashes.push(hash);
		if wait_finalized {
			loop {
				let block = blocks.next().await.context("finalized block stream ended")??;
				let body = client.body(block.hash().0).await?;
				let Some(index) = body.iter().position(|bytes| {
					hex::encode(polkameter_plugin_sdk::chain::tx_hash(bytes)) == strip_0x(&tx.hash)
				}) else {
					continue;
				};
				let index = index as u32;
				let events =
					polkameter_plugin_sdk::chain::events(&client.at(block.hash().0).await?).await?;
				ensure!(
					!polkameter_plugin_sdk::chain::failed_extrinsics(&events).contains(&index),
					"setup extrinsic failed"
				);
				if tx.metadata.get("sudo") == Some(&Value::Bool(true)) {
					ensure!(
						events.iter().any(|e| e.extrinsic == Some(index)
							&& e.is("Sudo", "Sudid")
							&& polkameter_plugin_sdk::chain::value::nth(&e.fields, 0)
								.and_then(polkameter_plugin_sdk::chain::value::variant_name)
								== Some("Ok")),
						"sudo setup failed"
					);
				}
				finalized_at = hex::encode(block.hash().0);
				break;
			}
		}
	}
	Ok(json!({"hashes": hashes, "at": finalized_at}))
}

/// Check results of the evaluate steps whose operation declares a `checks` output, in plan order.
/// A step that failed returned nothing. The report redacts the results before it renders, prints
/// or stores them.
fn plugin_checks(
	plan: &Plan,
	plugins: &Plugins,
	values: &Values,
) -> Result<Vec<polkameter_plugin_sdk::observers::CheckResult>> {
	let mut out = Vec::new();
	for step in &plan.evaluate.entries {
		if !contract(plugins, &step.operation)?.outputs.contains_key("checks") {
			continue;
		}
		let Some(value) = values.get(&format!("steps.{}", step.id)) else { continue };
		let mut checks =
			Vec::<polkameter_plugin_sdk::observers::CheckResult>::deserialize(&value["checks"])
				.with_context(|| format!("step {} returned malformed checks", step.id))?;
		for check in &mut checks {
			check.source = Some(step.id.clone());
		}
		out.extend(checks);
	}
	Ok(out)
}

fn validate_interval(configured: f64, measured: f64) -> Result<()> {
	ensure!(
		measured.is_finite()
			&& measured > 0.0
			&& (measured - configured).abs() <= configured * 0.25,
		"measured block interval {measured:.3}s differs from configured {configured:.3}s by more than 25%; correct the plan before setup"
	);
	Ok(())
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn a_failed_phase_sets_the_exit_code_and_whether_teardown_runs() {
		let setup_fails = r#"<polkameter-plan xmlns="https://polkameter.dev/schema/plan" version="1" name="Setup">
            <setup><step id="prepare" use="core.assert-equal"><input name="actual" value="1"/><input name="expected" value="2"/></step></setup>
            <teardown><step id="cleanup" use="core.echo"><input name="value" value="done"/></step></teardown>
        </polkameter-plan>"#;
		// No chain answers, so calibration fails in preflight: setup must not run, nothing to tear down.
		let calibration_fails = r#"<polkameter-plan xmlns="https://polkameter.dev/schema/plan" version="1" name="Calibration" timeout-ms="5000">
            <targets><target id="chain" endpoint="ws://127.0.0.1:0"/></targets>
            <setup><step id="prepare" use="core.echo"><input name="value" value="[]"/></step></setup>
            <load target="chain" source-ref="steps.prepare.value" probes-ref="steps.prepare.value">
                <rate tx-per-second="1" seconds="1"/>
            </load>
            <teardown><step id="cleanup" use="core.echo"><input name="value" value="done"/></step></teardown>
        </polkameter-plan>"#;
		let cases = [
			(setup_fails, 1, Some(Teardown::Completed), "setup", true),
			(calibration_fails, PREFLIGHT_EXIT_CODE, None, "preflight", false),
		];
		for (xml, exit_code, teardown, phase, setup_ran) in cases {
			let root = std::env::temp_dir().join(format!(
				"polkameter-failed-phase-{}-{}",
				std::process::id(),
				next_directory_sequence()
			));
			let plan = Plan::parse(xml).unwrap();
			let outcome = run_stoppable(
				plan,
				Registry::default(),
				&root,
				Stop::default(),
				Arc::new(|_| {}),
				RunOptions::default(),
			)
			.await
			.unwrap();
			assert_eq!(
				(outcome.exit_code, outcome.state),
				(exit_code, RunState::Failed),
				"{phase}"
			);
			assert_eq!(outcome.teardown, teardown, "{phase}");
			assert_eq!(outcome.failed_phase.as_deref(), Some(phase));
			let recorded: Value = serde_json::from_slice(
				&std::fs::read(outcome.artifact_dir.join("execution.json")).unwrap(),
			)
			.unwrap();
			assert_eq!(recorded["exit_code"], exit_code, "{phase}");
			assert_eq!(recorded["failed_phase"], phase);
			let events =
				std::fs::read_to_string(outcome.artifact_dir.join("events.jsonl")).unwrap();
			let steps: Vec<Value> = events
				.lines()
				.map(|line| serde_json::from_str::<Value>(line).unwrap()["step"].clone())
				.collect();
			assert_eq!(steps.contains(&json!("prepare")), setup_ran, "{phase}");
			assert_eq!(steps.contains(&json!("cleanup")), setup_ran, "{phase}");
			std::fs::remove_dir_all(root).unwrap();
		}
	}

	#[test]
	fn a_preflight_step_that_changes_state_is_named() {
		let xml = "<polkameter-plan xmlns=\"https://polkameter.dev/schema/plan\" version=\"1\" \
		           name=\"x\"><preflight><step id=\"send\" use=\"core.submit-prepared\"><input \
		           name=\"target\" value=\"ws://x\"/><input name=\"transactions\" \
		           value=\"[]\"/></step></preflight></polkameter-plan>";
		let plan = Plan::parse(xml).unwrap();
		let error = validate_contracts(&plan, &Plugins::default()).unwrap_err().to_string();
		assert!(
			error.contains(
				"step `send` (core.submit-prepared): this operation can change chain state, so it \
				 cannot run in <preflight>; move the step to <setup>"
			),
			"{error}"
		);
	}

	#[test]
	fn a_credential_longer_than_the_assertion_limit_leaks_no_prefix() {
		let secret = "abcdefghij".repeat(20);
		let value = assertion_value(
			&json!(format!("{secret}\n tail")),
			&Redactor::new([secret.clone()], None::<&str>),
		);
		assert!(!value.contains(&secret[..20]), "{value}");
	}
}
