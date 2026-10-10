//! Execution of declarative steps through the same host in every frontend.
use crate::{
	artifacts, measurement,
	plan::{InputSource, Plan, Step, Workflow},
	plugins::{Plugins, Registry, checksum},
	wiring,
};
use anyhow::{Context as _, Result, bail, ensure};
use polkameter_files::RunDir;
use polkameter_monitors::Job;
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
}
/// How a run ended. The serialized names are part of `execution.json`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
	Completed,
	CompletedWithFailures,
	Stopped,
	Failed,
}
impl std::fmt::Display for RunState {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.write_str(match self {
			Self::Completed => "completed",
			Self::CompletedWithFailures => "completed_with_failures",
			Self::Stopped => "stopped",
			Self::Failed => "failed",
		})
	}
}

/// Event writer shared by every task of a run. Events are redacted before they are persisted.
#[derive(Clone)]
struct Log {
	file: Arc<Mutex<File>>,
	sink: EventSink,
	secrets: Arc<Vec<String>>,
}
impl Log {
	fn open(directory: &Path, sink: EventSink, secrets: Vec<String>) -> Result<Self> {
		let file = OpenOptions::new()
			.create_new(true)
			.write(true)
			.open(directory.join("events.jsonl"))?;
		Ok(Self { file: Arc::new(Mutex::new(file)), sink, secrets: Arc::new(secrets) })
	}
	fn emit(&self, mut value: Value) -> Result<()> {
		value["timestamp"] = json!(polkameter_files::now_ms());
		value["version"] = json!(2);
		redact(&mut value, &self.secrets);
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
fn redact_text(text: &str, secrets: &[String]) -> String {
	secrets
		.iter()
		.fold(text.to_owned(), |text, secret| text.replace(secret, "[redacted]"))
}
fn redact(value: &mut Value, secrets: &[String]) {
	match value {
		Value::String(text) => *text = redact_text(text, secrets),
		Value::Array(items) => items.iter_mut().for_each(|item| redact(item, secrets)),
		Value::Object(items) => items.values_mut().for_each(|item| redact(item, secrets)),
		_ => {},
	}
}
fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
	std::fs::write(path, serde_json::to_vec_pretty(value)?)?;
	Ok(())
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
fn contract<'a>(plugins: &'a Plugins, name: &str) -> Result<Cow<'a, Operation>> {
	if name.starts_with("core.") {
		return core_contract(name)
			.map(Cow::Owned)
			.with_context(|| format!("unknown built-in operation {name}"));
	}
	let (plugin, operation) = name.split_once('.').context("invalid operation")?;
	Ok(Cow::Borrowed(plugins.operation(plugin, operation)?))
}
/// Validate operation names, literals, references and output types before any mutation.
fn validate_contracts(plan: &Plan, plugins: &Plugins) -> Result<()> {
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
	types.extend([
		("user.index".into(), Schema::Integer),
		("iteration.index".into(), Schema::Integer),
	]);
	for step in plan.all_steps() {
		let operation = contract(plugins, &step.operation)?;
		let input_names =
			step.inputs.iter().map(|input| input.name.as_str()).collect::<BTreeSet<_>>();
		for (name, field) in &operation.inputs {
			ensure!(
				field.optional || input_names.contains(name.as_str()),
				"step {} misses input {name}",
				step.id
			);
		}
		for input in &step.inputs {
			let expected = operation
				.inputs
				.get(&input.name)
				.with_context(|| format!("unknown input {} for {}", input.name, step.operation))?;
			if let InputSource::Ref(reference) = &input.source {
				let actual =
					types.get(reference).with_context(|| format!("unknown output {reference}"))?;
				ensure!(
					actual == &expected.schema
						|| matches!(actual, Schema::Json)
						|| matches!(expected.schema, Schema::Json),
					"reference {reference} has incompatible type"
				);
			} else {
				expected.schema.validate(&input.literal()?)?;
			}
		}
		for (name, field) in &operation.outputs {
			types.insert(format!("steps.{}.{name}", step.id), field.schema.clone());
		}
	}
	for step in &plan.preflight.entries {
		ensure!(
			contract(plugins, &step.operation)?.read_only,
			"preflight operation {} can mutate state",
			step.operation
		);
	}
	if let Some(load) = &plan.load {
		ensure!(
			types.contains_key(&load.source) && types.contains_key(&load.probes_ref),
			"unknown load source output"
		);
		if let Some(state) = &load.state {
			contract(plugins, &state.check)?;
		}
	}
	Ok(())
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
		polkameter_files::now_ms(),
		next_directory_sequence()
	));
	std::fs::create_dir(&directory)?;
	let _scratch = Scratch(directory.clone());
	let context = Context::host(purpose, &directory);
	let plugins = Plugins::start(&plan.plugins.entries, registry, &context).await?;
	// Boxed: the bodies are large and would otherwise sit on the caller's stack.
	let result = Box::pin(body(&plugins, &context, &directory)).await;
	plugins.shutdown(&context).await;
	result
}
pub async fn inspect(plan: &Plan, registry: &Registry) -> Result<Value> {
	plan.validate()?;
	with_scratch_plugins(plan, registry, "inspect", async |plugins, _, _| {
		validate_contracts(plan, plugins)?;
		let requirements: Vec<_> =
			plugins.manifests.values().flat_map(|m| m.requirements.clone()).collect();
		let metric_catalog: Vec<_> = polkameter_files::registry::NODE_METRICS
			.iter()
			.map(
				|m| json!({"name": m.name, "description": m.help, "labels": m.labels, "roles": m.from}),
			)
			.collect();
		Ok(json!({
			"valid": true,
			"plugins": plugins.manifests,
			"requirements": requirements,
			"targets": plan.targets,
			"monitorRequirements": plan.monitors,
			"metricCatalog": metric_catalog,
		}))
	})
	.await
}
pub async fn preflight(plan: &Plan, registry: &Registry) -> Result<Value> {
	let mut inspected = inspect(plan, registry).await?;
	// Preflight never arms setup or load and writes only to its scratch directory.
	let calibration =
		with_scratch_plugins(plan, registry, "preflight", async |plugins, context, directory| {
			check_environment(plan, registry).await?;
			check_requirements(plan, registry, plugins).await?;
			let mut values = plan.values("preflight");
			let mut secrets = Vec::new();
			// Resolve credentials only if an explicit preflight step references one.
			if plan
				.preflight
				.entries
				.iter()
				.flat_map(|s| &s.inputs)
				.any(|i| matches!(&i.source, InputSource::Ref(r) if r.starts_with("credentials.")))
			{
				let credentials = registry.resolve_credentials(plan)?;
				secrets = secret_values(&credentials);
				values.extend(credentials);
			}
			for step in &plan.preflight.entries {
				// Plugin errors may echo a credential, so they are redacted as the run's are.
				let output = invoke(step, plugins, &values, context, &CancellationToken::new())
					.await
					.map_err(|e| anyhow::anyhow!(redact_text(&format!("{e:#}"), &secrets)))?;
				values.insert(format!("steps.{}", step.id), output);
			}
			calibrate(plan, directory).await
		})
		.await?;
	if let Some(calibration) = calibration {
		inspected["calibration"] = calibration;
	}
	Ok(inspected)
}
async fn check_environment(plan: &Plan, registry: &Registry) -> Result<()> {
	let (Some(monitor), Some(targets)) = (&plan.monitors, wiring::monitor_targets(plan, registry)?)
	else {
		return Ok(());
	};
	for warning in polkameter_monitors::preflight::preflight(&targets).await? {
		eprintln!("preflight: {warning}");
	}
	let metrics = monitor
		.metrics
		.iter()
		.map(|m| Ok((m.role.parse::<Job>()?, m.name.clone())))
		.collect::<Result<Vec<_>>>()?;
	polkameter_monitors::preflight::require_metrics(&targets, &metrics).await?;
	Ok(())
}
/// Calibrate during preflight, before any recognition, signing or proving in setup.
async fn calibrate(plan: &Plan, directory: &Path) -> Result<Option<Value>> {
	let Some(load) = &plan.load else { return Ok(None) };
	let endpoint = target_endpoint(plan, &load.target)?;
	let measured = async {
		let client = polkameter_chain::Client::connect(endpoint).await?;
		client.block_interval_s().await
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
	write_json(&directory.join("calibration.json"), &calibration)?;
	validate_interval(load.block_interval_seconds, measured)?;
	Ok(Some(calibration))
}

/// Runs a plan in a new directory under `root`. Phases: preflight, setup, workflows, measurement,
/// evaluate, then teardown, which runs whenever the plugins started. The outcome is written to the
/// run directory and returned; its exit code is 130 when the run was cancelled.
pub async fn run(
	plan: Plan,
	registry: Registry,
	root: &Path,
	cancel: CancellationToken,
	sink: EventSink,
) -> Result<Outcome> {
	plan.validate()?;
	// A new `run-…` directory under `root`: a run never reuses one.
	std::fs::create_dir_all(root)?;
	let run_id = format!(
		"run-{}-{}-{}",
		polkameter_files::now_ms(),
		std::process::id(),
		next_directory_sequence()
	);
	let directory = root.join(&run_id);
	std::fs::create_dir(&directory).context("run directory must be new")?;
	let credentials = registry.resolve_credentials(&plan);
	let log =
		Log::open(&directory, sink, credentials.as_ref().map(secret_values).unwrap_or_default())?;
	write_json(&directory.join("plan.json"), &plan)?;
	let context = Context::host(run_id, directory.canonicalize()?);
	let run = Run {
		plan: &plan,
		registry: &registry,
		context,
		directory,
		cancel,
		log,
		started: Instant::now(),
	};
	let result = Box::pin(run.execute(credentials)).await;
	run.outcome(result)
}

/// One run in progress: its plan and host, its directory and event log, and its deadline.
struct Run<'a> {
	plan: &'a Plan,
	registry: &'a Registry,
	context: Context,
	directory: PathBuf,
	cancel: CancellationToken,
	log: Log,
	started: Instant,
}

impl Run<'_> {
	/// Starts the plugins, runs the phases under the whole-run deadline, then tears down.
	async fn execute(&self, credentials: Result<Values>) -> Result<i32> {
		let credentials = credentials?;
		let plugins = self.start_plugins().await?;
		let mut values = self.values(credentials);
		let result = self.bounded(Box::pin(self.phases(&plugins, &mut values))).await;
		let cleanup = self.teardown(&plugins, &mut values).await;
		plugins.shutdown(&self.context).await;
		let code = result?;
		cleanup?;
		Ok(code)
	}

	async fn start_plugins(&self) -> Result<Plugins> {
		cancellable(&self.cancel, async {
			tokio::time::timeout(
				Duration::from_millis(self.plan.timeout_ms),
				Plugins::start(&self.plan.plugins.entries, self.registry, &self.context),
			)
			.await
			.context("whole-run deadline exceeded during plugin startup")?
		})
		.await
	}

	fn values(&self, credentials: Values) -> Values {
		let mut values = self.plan.values(&self.context.run_id);
		values.extend(credentials);
		values.insert("run.directory".into(), json!(self.context.artifact_dir));
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
		self.log.phase("preflight")?;
		check_environment(self.plan, self.registry).await?;
		check_requirements(self.plan, self.registry, plugins).await?;
		self.run_steps(&self.plan.preflight.entries, plugins, values).await?;
		calibrate(self.plan, &self.directory).await?;
		self.log.phase("setup")?;
		self.run_steps(&self.plan.setup.entries, plugins, values).await?;
		for workflow in &self.plan.workflows {
			self.workflow(plugins, values, workflow).await?;
		}
		let measured = match &self.plan.load {
			Some(_) => {
				self.log.phase("measurement")?;
				let setup_seconds = self.started.elapsed().as_secs();
				let progress = |phase: &str| self.log.phase(phase);
				Some(
					measurement::run(
						self.plan,
						plugins,
						self.registry,
						values,
						&self.context,
						setup_seconds,
						&progress,
					)
					.await?,
				)
			},
			None => None,
		};
		let evaluated = self.run_steps(&self.plan.evaluate.entries, plugins, values).await;
		// The report keeps the built-in evidence even when a plugin check is malformed; that error
		// is the run's, after the report is written.
		let mut checks_error = None;
		let code = match measured {
			Some(measured) => {
				self.log.phase("checks")?;
				let dir = RunDir::open(&self.context.artifact_dir);
				let checks = plugin_checks(self.plan, plugins, values).unwrap_or_else(|e| {
					checks_error = Some(e);
					Vec::new()
				});
				measurement::report(&dir, measured, &checks)?
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
		write_json(&self.directory.join("resolved-plan.json"), &resolved)?;
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
				.map(|t| json!({"role": t.job.label(), "instance": t.instance, "url": t.url}))
				.collect();
			let resolved = json!({
				"topologyHash": checksum(topology)?,
				"targets": target_records,
			});
			write_json(&self.directory.join("resolved-targets.json"), &resolved)?;
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

	/// Each user runs its iterations concurrently with the other users, up to the workflow's
	/// concurrency. A user's iterations run in order, each on a copy of the user's values.
	async fn workflow(
		&self,
		plugins: &Plugins,
		values: &Values,
		workflow: &Workflow,
	) -> Result<()> {
		self.log
			.emit(json!({"event": "phase", "phase": "workflow", "id": workflow.id}))?;
		let steps = Arc::new(workflow.steps.clone());
		let mut tasks = tokio::task::JoinSet::new();
		for user in 0..workflow.users {
			if tasks.len() >= workflow.concurrency as usize {
				tasks.join_next().await.context("workflow join missing")???;
			}
			let (plugins, log, cancel, steps) =
				(plugins.clone(), self.log.clone(), self.cancel.clone(), Arc::clone(&steps));
			let (mut context, mut user_values) = (self.context.clone(), values.clone());
			let iterations = workflow.iterations;
			tasks.spawn(async move {
				context.user = Some(user);
				user_values.insert("user.index".into(), json!(user));
				for iteration in 0..iterations {
					// Iteration outputs cannot leak into the next iteration.
					let mut iteration_values = user_values.clone();
					context.iteration = Some(iteration);
					iteration_values.insert("iteration.index".into(), json!(iteration));
					execute_steps(&steps, &plugins, &mut iteration_values, &context, &cancel, &log)
						.await?;
				}
				Ok::<_, anyhow::Error>(())
			});
		}
		while let Some(task) = tasks.join_next().await {
			task??;
		}
		Ok(())
	}

	/// Teardown runs even after a failure or cancellation, under its own deadline.
	async fn teardown(&self, plugins: &Plugins, values: &mut Values) -> Result<()> {
		self.log.phase("teardown")?;
		let cleanup = tokio::time::timeout(
			Duration::from_secs(30),
			cleanup_steps(&self.plan.teardown.entries, plugins, values, &self.context, &self.log),
		)
		.await
		.map_err(|_| anyhow::anyhow!("teardown deadline exceeded"))
		.and_then(|r| r);
		if let Err(error) = &cleanup {
			self.log.emit(json!({"event": "cleanup-failed", "error": error.to_string()}))?;
		}
		cleanup
	}

	/// Settles how the run ended, writes its artifacts and outcome, and logs the outcome.
	fn outcome(&self, result: Result<i32>) -> Result<Outcome> {
		let redacted = redact_plugin_logs(&self.directory, &self.log.secrets);
		let (exit_code, state, error) = match result {
			Ok(0) => (0, RunState::Completed, None),
			Ok(code) => (code, RunState::CompletedWithFailures, None),
			Err(error) if self.cancel.is_cancelled() => (130, RunState::Stopped, Some(error)),
			Err(error) => (1, RunState::Failed, Some(error)),
		};
		let mut outcome = Outcome {
			artifact_dir: self.context.artifact_dir.clone(),
			exit_code,
			state,
			error: error.map(|error| redact_text(&error.to_string(), &self.log.secrets)),
		};
		// Samples and plots come from the raw files.
		for error in [redacted, artifacts::write_outputs(&self.directory)]
			.into_iter()
			.filter_map(Result::err)
		{
			fail_report(&mut outcome, error);
		}
		write_json(&self.directory.join("execution.json"), &outcome)?;
		let summary = self.directory.join("summary.md");
		if !summary.exists() {
			std::fs::write(
				summary,
				format!(
					"# {}\n\nStatus: {}\n\n{}\n",
					self.plan.name,
					outcome.state,
					outcome.error.as_deref().unwrap_or("All configured steps completed.")
				),
			)?;
		}
		self.log.emit(json!({"event": "completed", "outcome": outcome}))?;
		Ok(outcome)
	}
}

/// Plugins write their stderr raw, straight to the file, so each log is redacted once the run is over.
fn redact_plugin_logs(directory: &Path, secrets: &[String]) -> Result<()> {
	let Ok(plugins) = std::fs::read_dir(directory.join("plugins")) else { return Ok(()) };
	for plugin in plugins {
		let log = plugin?.path().join("stderr.log");
		if let Ok(bytes) = std::fs::read(&log) {
			std::fs::write(&log, redact_text(&String::from_utf8_lossy(&bytes), secrets))?;
		}
	}
	Ok(())
}

/// A report that cannot be written fails the run. A cancelled run keeps its state, and the error
/// is appended to the run's error.
fn fail_report(outcome: &mut Outcome, error: impl std::fmt::Display) {
	let report = format!("report generation failed: {error}");
	if outcome.state != RunState::Stopped {
		outcome.exit_code = 1;
		outcome.state = RunState::Failed;
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
		let mut event = step_event("step-started", step, context);
		event["operation"] = json!(step.operation);
		log.emit(event)?;
		let result: Result<Value> =
			with_progress(step, started, log, invoke(step, plugins, values, context, cancel))
				.await?;
		let mut event = step_event("step-finished", step, context);
		event["elapsedMs"] = json!(started.elapsed().as_millis());
		event["success"] = json!(result.is_ok());
		event["error"] = json!(result.as_ref().err().map(ToString::to_string));
		log.emit(event)?;
		values.insert(format!("steps.{}", step.id), result?);
	}
	Ok(())
}

fn step_event(kind: &str, step: &Step, context: &Context) -> Value {
	json!({
		"event": kind,
		"step": step.id,
		"user": context.user,
		"iteration": context.iteration,
	})
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
) -> Result<Value> {
	let inputs = Value::Object(
		step.inputs
			.iter()
			.map(|input| {
				let value = match &input.source {
					InputSource::Ref(reference) => resolve(values, reference)?,
					_ => input.literal()?,
				};
				Ok((input.name.clone(), value))
			})
			.collect::<Result<serde_json::Map<_, _>>>()?,
	);
	let operation = contract(plugins, &step.operation)?;
	operation.validate_inputs(&inputs)?;
	if !step.operation.starts_with("core.") {
		return plugins.invoke(&step.operation, inputs, context, step.timeout_ms, cancel).await;
	}
	let work = Box::pin(core_operation(step, &inputs, context));
	cancellable(cancel, async {
		tokio::time::timeout(Duration::from_millis(step.timeout_ms), work)
			.await
			.map_err(|_| {
				anyhow::anyhow!("step {} timed out; submission outcome may be unknown", step.id)
			})?
	})
	.await
}

/// The built-in operations, which run in the host rather than in a plugin.
async fn core_operation(step: &Step, inputs: &Value, context: &Context) -> Result<Value> {
	match step.operation.as_str() {
		"core.echo" => Ok(json!({"value": inputs["value"]})),
		"core.assert-equal" => {
			ensure!(inputs["actual"] == inputs["expected"], "assertion {} failed", step.id);
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
	let client =
		polkameter_chain::Client::connect(inputs["target"].as_str().context("target missing")?)
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
					hex::encode(polkameter_chain::tx_hash(bytes)) == strip_0x(&tx.hash)
				}) else {
					continue;
				};
				let index = index as u32;
				let events = polkameter_chain::events(&client.at(block.hash().0).await?).await?;
				ensure!(
					!polkameter_chain::failed_extrinsics(&events).contains(&index),
					"setup extrinsic failed"
				);
				if tx.metadata.get("sudo") == Some(&Value::Bool(true)) {
					ensure!(
						events.iter().any(|e| e.extrinsic == Some(index)
							&& e.is("Sudo", "Sudid")
							&& polkameter_chain::value::nth(&e.fields, 0)
								.and_then(polkameter_chain::value::variant_name)
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
/// A step that failed returned nothing.
fn plugin_checks(
	plan: &Plan,
	plugins: &Plugins,
	values: &Values,
) -> Result<Vec<polkameter_checks::CheckResult>> {
	let mut out = Vec::new();
	for step in &plan.evaluate.entries {
		if !contract(plugins, &step.operation)?.outputs.contains_key("checks") {
			continue;
		}
		let Some(value) = values.get(&format!("steps.{}", step.id)) else { continue };
		let checks = Vec::<polkameter_checks::CheckResult>::deserialize(&value["checks"])
			.with_context(|| format!("step {} returned malformed checks", step.id))?;
		out.extend(checks);
	}
	Ok(out)
}

async fn check_requirements(plan: &Plan, registry: &Registry, plugins: &Plugins) -> Result<()> {
	for requirement in plugins
		.manifests
		.values()
		.flat_map(|manifest| &manifest.requirements)
		.filter(|requirement| requirement.required)
	{
		match requirement.kind.as_str() {
			"capability" => ensure!(
				registry.capabilities.contains(&requirement.name),
				"provisioner has not declared capability {}",
				requirement.name
			),
			"rpc" => {
				let endpoint = target_endpoint(plan, &requirement.target)?;
				let client = polkameter_chain::Client::connect(endpoint).await?;
				let methods: Value =
					client.request("rpc_methods", subxt_rpcs::rpc_params![]).await?;
				ensure!(
					methods["methods"].as_array().is_some_and(|methods| methods
						.iter()
						.any(|method| method.as_str() == Some(&requirement.name))),
					"required RPC method {} is missing",
					requirement.name
				);
			},
			"metric" => {
				let targets = wiring::monitor_targets(plan, registry)?
					.context("metric requirement needs a topology")?;
				let role = requirement.target.parse::<Job>()?;
				polkameter_monitors::preflight::require_metrics(
					&targets,
					&[(role, requirement.name.clone())],
				)
				.await?;
			},
			other => bail!("unsupported required evidence kind {other}"),
		}
	}
	Ok(())
}

/// Runs every teardown step even after a failure, then reports the first error.
async fn cleanup_steps(
	steps: &[Step],
	plugins: &Plugins,
	values: &mut Values,
	context: &Context,
	log: &Log,
) -> Result<()> {
	let mut result = Ok(());
	for step in steps {
		// Teardown is not cancelled with the run: it is what makes the run's outputs safe to keep.
		let step_result = execute_steps(
			std::slice::from_ref(step),
			plugins,
			values,
			context,
			&CancellationToken::new(),
			log,
		)
		.await;
		result = result.and(step_result);
	}
	result
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
mod calibration_tests {
	use super::*;

	#[test]
	fn rejects_wrong_topology_and_unusable_measurements() {
		for measured in [2.0, 0.0, f64::NAN, f64::INFINITY] {
			assert!(super::validate_interval(6.0, measured).is_err());
		}
		assert!(super::validate_interval(2.0, 2.1).is_ok());
		assert!(super::validate_interval(2.0, 1.5).is_ok());
		assert!(super::validate_interval(2.0, 2.5).is_ok());
		assert!(super::validate_interval(2.0, 2.51).is_err());
	}

	#[tokio::test]
	async fn calibration_failure_prevents_setup_and_still_runs_teardown() {
		let plan = Plan::parse(r#"<polkameter-plan xmlns="https://polkameter.dev/schema/plan" version="1" name="Calibration" timeout-ms="5000">
            <targets><target id="chain" endpoint="ws://127.0.0.1:0"/></targets>
            <setup><step id="prepare" use="core.echo"><input name="value" value="[]"/></step></setup>
            <load target="chain" source-ref="steps.prepare.value" probes-ref="steps.prepare.value">
                <rate tx-per-second="1" seconds="1"/>
            </load>
            <teardown><step id="cleanup" use="core.echo"><input name="value" value="done"/></step></teardown>
        </polkameter-plan>"#).unwrap();
		let registry = Registry::default();
		let error = preflight(&plan, &registry).await.unwrap_err();
		assert!(error.to_string().contains("block interval calibration failed during preflight"));
		let root = std::env::temp_dir().join(format!(
			"polkameter-calibration-{}-{}",
			std::process::id(),
			next_directory_sequence()
		));
		let outcome = run(plan, registry, &root, CancellationToken::new(), Arc::new(|_| {}))
			.await
			.unwrap();
		assert_eq!(outcome.exit_code, 1);
		assert!(
			outcome
				.error
				.unwrap()
				.contains("block interval calibration failed during preflight")
		);
		let events: Vec<Value> = std::fs::read_to_string(outcome.artifact_dir.join("events.jsonl"))
			.unwrap()
			.lines()
			.map(|line| serde_json::from_str(line).unwrap())
			.collect();
		assert!(
			!events
				.iter()
				.any(|event| event["phase"] == "setup" || event["step"] == "prepare")
		);
		assert!(events.iter().any(|event| event["step"] == "cleanup"
			&& event["event"] == "step-finished"
			&& event["success"] == true));
		std::fs::remove_dir_all(root).unwrap();
	}

	#[test]
	fn a_failed_report_keeps_a_cancelled_run_stopped() {
		let mut outcome = Outcome {
			artifact_dir: PathBuf::new(),
			exit_code: 130,
			state: RunState::Stopped,
			error: Some("run cancelled".into()),
		};
		fail_report(&mut outcome, "disk full");
		assert_eq!((outcome.exit_code, outcome.state), (130, RunState::Stopped));
		assert_eq!(
			outcome.error.as_deref(),
			Some("run cancelled; report generation failed: disk full")
		);
	}

	#[tokio::test]
	async fn environment_checks_run_before_the_preflight_steps() {
		let plan = Plan::parse(r#"<polkameter-plan xmlns="https://polkameter.dev/schema/plan" version="1" name="Environment" timeout-ms="5000">
            <targets><target id="chain" endpoint="ws://127.0.0.1:0"/></targets>
            <monitors topology="not-installed" para-id="1" relay-target="chain"/>
            <setup><step id="prepare" use="core.echo"><input name="value" value="[]"/></step></setup>
            <load target="chain" source-ref="steps.prepare.value" probes-ref="steps.prepare.value">
                <rate tx-per-second="1" seconds="1"/>
            </load>
            <preflight><step id="check" use="core.assert-equal"><input name="actual" value="1"/><input name="expected" value="2"/></step></preflight>
        </polkameter-plan>"#).unwrap();
		let error = preflight(&plan, &Registry::default()).await.unwrap_err();
		assert!(
			error.to_string().contains("topology alias not-installed is not installed"),
			"{error:#}"
		);
	}
}
