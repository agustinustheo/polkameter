//! Execution of declarative steps through the same host in every frontend.
use crate::{
	plan::{Plan, Step},
	plugins::{Plugins, Registry},
};
use anyhow::{Context as _, Result, ensure};
use polkameter_plugin_sdk::{Artifact, Context, Field, Operation, PreparedTx, Schema};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
	collections::BTreeMap,
	fs::{File, OpenOptions},
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
	pub state: String,
	pub error: Option<String>,
}
#[derive(Clone)]
struct Log {
	file: Arc<Mutex<File>>,
	sink: EventSink,
	secrets: Arc<Vec<String>>,
}
impl Log {
	fn emit(&self, event: Value) -> Result<()> {
		fn redact(value: &mut Value, secrets: &[String]) {
			match value {
				Value::String(text) => {
					for secret in secrets {
						*text = text.replace(secret, "[redacted]");
					}
				},
				Value::Array(items) => {
					for item in items {
						redact(item, secrets);
					}
				},
				Value::Object(items) => {
					for item in items.values_mut() {
						redact(item, secrets);
					}
				},
				_ => {},
			}
		}
		let mut value = event;
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
fn builtin(name: &str) -> Result<Operation> {
	type Fields = Vec<(&'static str, Schema)>;
	let (inputs, outputs, read_only): (Fields, Fields, bool) = match name {
		"core.echo" => (vec![("value", Schema::Json)], vec![("value", Schema::Json)], true),
		"core.assert-equal" => (
			vec![("actual", Schema::Json), ("expected", Schema::Json)],
			vec![("passed", Schema::Boolean)],
			true,
		),
		"core.submit-prepared" | "core.submit-setup" => (
			vec![("target", Schema::String), ("transactions", Schema::Json)],
			vec![
				("hashes", Schema::Array { items: Box::new(Schema::String) }),
				("at", Schema::String),
			],
			false,
		),
		_ => anyhow::bail!("unknown built-in operation {name}"),
	};
	Ok(Operation {
		description: name.into(),
		inputs: inputs.into_iter().map(|(k, v)| (k.into(), Field::from(v))).collect(),
		outputs: outputs.into_iter().map(|(k, v)| (k.into(), Field::from(v))).collect(),
		read_only,
	})
}
fn contract(plugins: &Plugins, name: &str) -> Result<Operation> {
	if name.starts_with("core.") { builtin(name) } else { Ok(plugins.operation(name)?.clone()) }
}
/// Validate operation names, literals, references and output types before any mutation.
pub fn validate_contracts(plan: &Plan, plugins: &Plugins) -> Result<()> {
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
		let input_names = step
			.inputs
			.iter()
			.map(|input| input.name.as_str())
			.collect::<std::collections::BTreeSet<_>>();
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
			if let Some(reference) = &input.reference {
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
		if let Some(operation) = &load.state_check {
			plugins.operation(operation)?;
		}
	}
	Ok(())
}
fn next_directory_sequence() -> u64 {
	static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
	NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}
pub async fn inspect(plan: &Plan, registry: &Registry) -> Result<Value> {
	plan.validate()?;
	let directory = std::env::temp_dir().join(format!(
		"polkameter-inspect-{}-{}-{}",
		std::process::id(),
		polkameter_files::now_ms(),
		next_directory_sequence()
	));
	std::fs::create_dir(&directory)?;
	let context = Context {
		run_id: "inspect".into(),
		artifact_dir: directory.clone(),
		user: None,
		iteration: None,
	};
	let result=async {
        let plugins=Plugins::start(&plan.plugins.entries,registry,&context).await?;
        let result=validate_contracts(plan,&plugins).map(|_|json!({"valid":true,"plugins":plugins.manifests,"requirements":plugins.manifests.values().flat_map(|m|m.requirements.clone()).collect::<Vec<_>>(),"targets":plan.targets,"monitorRequirements":plan.monitors,"metricCatalog":polkameter_files::registry::NODE_METRICS.iter().map(|m|json!({"name":m.name,"description":m.help,"labels":m.labels,"roles":m.from})).collect::<Vec<_>>()}));
        plugins.shutdown(&context).await;
        result
    }.await;
	let _ = std::fs::remove_dir_all(directory);
	result
}
pub async fn preflight(plan: &Plan, registry: &Registry) -> Result<Value> {
	let mut inspected = inspect(plan, registry).await?;
	// Use a separate temporary directory; read-only preflight never arms setup/load.
	let directory = std::env::temp_dir().join(format!(
		"polkameter-preflight-{}-{}",
		std::process::id(),
		polkameter_files::now_ms()
	));
	std::fs::create_dir(&directory)?;
	let context = Context {
		run_id: "preflight".into(),
		artifact_dir: directory.clone(),
		user: None,
		iteration: None,
	};
	let result = async {
		let plugins = Plugins::start(&plan.plugins.entries, registry, &context).await?;
		let mut values = plan.values("preflight");
		// Resolve credentials only if an explicit preflight step references one.
		if plan
			.preflight
			.entries
			.iter()
			.flat_map(|s| &s.inputs)
			.any(|i| i.reference.as_deref().is_some_and(|r| r.starts_with("credentials.")))
		{
			values.extend(registry.resolve_credentials(plan)?);
		}
		let result = async {
			for step in &plan.preflight.entries {
				let output =
					invoke(step, &plugins, &values, &context, &CancellationToken::new()).await?;
				values.insert(format!("steps.{}", step.id), output);
			}
			check_environment(plan, registry).await?;
			check_requirements(plan, registry, &plugins).await?;
			if let Some(calibration) = calibrate(plan, &directory).await? {
				inspected["calibration"] = calibration;
			}
			Ok(inspected)
		}
		.await;
		plugins.shutdown(&context).await;
		result
	}
	.await;
	let _ = std::fs::remove_dir_all(directory);
	result
}
pub async fn check_environment(plan: &Plan, registry: &Registry) -> Result<()> {
	if let Some(monitor) = &plan.monitors {
		let path = registry
			.topologies
			.get(&monitor.topology)
			.context("topology alias is unavailable on this host")?;
		let targets = polkameter_monitors::load_targets(path, monitor.para_id)?;
		let missing = polkameter_monitors::preflight::preflight(&targets).await?;
		for warning in missing {
			eprintln!("preflight: {warning}");
		}
		polkameter_monitors::preflight::require_metrics(
			&targets,
			&monitor
				.metrics
				.iter()
				.map(|m| (m.role.clone(), m.name.clone()))
				.collect::<Vec<_>>(),
		)
		.await?;
	}
	Ok(())
}
/// Calibrate during preflight, before any recognition, signing or proving in setup.
async fn calibrate(plan: &Plan, directory: &Path) -> Result<Option<Value>> {
	let Some(load) = &plan.load else { return Ok(None) };
	let target = plan
		.targets
		.entries
		.iter()
		.find(|target| target.id == load.target)
		.context("load target missing")?;
	let measured = async {
		let client = polkameter_chain::Client::connect(&target.endpoint).await?;
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
	std::fs::write(directory.join("calibration.json"), serde_json::to_vec_pretty(&calibration)?)?;
	validate_interval(load.block_interval_seconds, measured)?;
	Ok(Some(calibration))
}
pub async fn run(
	plan: Plan,
	registry: Registry,
	root: &Path,
	cancel: CancellationToken,
	sink: EventSink,
) -> Result<Outcome> {
	plan.validate()?;
	std::fs::create_dir_all(root)?;
	let run_id = format!(
		"run-{}-{}-{}",
		polkameter_files::now_ms(),
		std::process::id(),
		next_directory_sequence()
	);
	let directory = root.join(&run_id);
	std::fs::create_dir(&directory).context("run directory must be new")?;
	let context = Context {
		run_id: run_id.clone(),
		artifact_dir: directory.canonicalize()?,
		user: None,
		iteration: None,
	};
	let credentials = registry.resolve_credentials(&plan);
	let secrets = Arc::new(
		credentials
			.as_ref()
			.ok()
			.into_iter()
			.flat_map(|values| values.values())
			.filter_map(|v| v.as_str().map(str::to_owned))
			.collect::<Vec<_>>(),
	);
	let log = Log {
		file: Arc::new(Mutex::new(
			OpenOptions::new()
				.create_new(true)
				.write(true)
				.open(directory.join("events.jsonl"))?,
		)),
		sink,
		secrets,
	};
	std::fs::write(directory.join("plan.json"), serde_json::to_vec_pretty(&plan)?)?;
	let context_for_tasks = context.clone();
	let preparation_started = Instant::now();
	let result=async {
        let credentials=credentials?;
        let plugins=tokio::select! {
            biased;
            _=cancel.cancelled()=>anyhow::bail!("run cancelled"),
            result=tokio::time::timeout(Duration::from_millis(plan.timeout_ms),Plugins::start(&plan.plugins.entries,&registry,&context))=>result.context("whole-run deadline exceeded during plugin startup")??,
        };
        let mut values=plan.values(&run_id);values.extend(credentials);
        values.insert("run.directory".into(),json!(context.artifact_dir));
        let main=async {
            validate_contracts(&plan,&plugins)?;
            let requirements=json!({"plugins":plugins.manifests,"topologies":plan.monitors,"installations":registry.plugins.iter().filter(|(id,_)|plugins.manifests.contains_key(*id)).collect::<BTreeMap<_,_>>(),"host":{"engineVersion":env!("CARGO_PKG_VERSION"),"blake2":crate::plugins::checksum(&std::env::current_exe()?)?}});
            std::fs::write(directory.join("resolved-plan.json"),serde_json::to_vec_pretty(&requirements)?)?;
            if let Some(monitors)=&plan.monitors {
                let topology=registry.topologies.get(&monitors.topology).context("topology is not installed")?;
                let targets=polkameter_monitors::load_targets(topology,monitors.para_id)?;
                std::fs::write(directory.join("resolved-targets.json"),serde_json::to_vec_pretty(&json!({"topologyHash":crate::plugins::checksum(topology)?,"targets":targets.iter().map(|t|json!({"role":t.job.label(),"instance":t.instance,"url":t.url})).collect::<Vec<_>>()}))?)?;
            }
            log.emit(json!({"event":"phase","phase":"preflight"}))?;
            check_environment(&plan,&registry).await?;
            check_requirements(&plan,&registry,&plugins).await?;
            execute_steps(&plan.preflight.entries,&plugins,&mut values,&context,&cancel,&log).await?;
            calibrate(&plan,&directory).await?;
            log.emit(json!({"event":"phase","phase":"setup"}))?;
            execute_steps(&plan.setup.entries,&plugins,&mut values,&context,&cancel,&log).await?;
            for workflow in &plan.workflows {
                log.emit(json!({"event":"phase","phase":"workflow","id":workflow.id}))?;
                let mut tasks=tokio::task::JoinSet::new();
                for user in 0..workflow.users {
                    if tasks.len()>=workflow.concurrency as usize {tasks.join_next().await.context("workflow join missing")???;}
                    let (plugins,mut local,mut context,cancel,log,steps,iterations)=(plugins.clone(),values.clone(),context_for_tasks.clone(),cancel.clone(),log.clone(),workflow.steps.clone(),workflow.iterations);
                    tasks.spawn(async move {
                        context.user=Some(user);local.insert("user.index".into(),json!(user));
                        for iteration in 0..iterations {
                            // Iteration outputs cannot leak into the next iteration.
                            let mut iteration_values=local.clone();context.iteration=Some(iteration);
                            iteration_values.insert("iteration.index".into(),json!(iteration));
                            execute_steps(&steps,&plugins,&mut iteration_values,&context,&cancel,&log).await?;
                        }
                        Ok::<_,anyhow::Error>(())
                    });
                }
                while let Some(task)=tasks.join_next().await {task??;}
            }
            let dir=polkameter_files::RunDir::open(&context.artifact_dir);
            let measured=match &plan.load {
                Some(_)=>{
                    log.emit(json!({"event":"phase","phase":"measurement"}))?;
                    Some(crate::measurement::run(&plan,&plugins,&registry,&values,&context,preparation_started.elapsed().as_secs(),&|phase|log.emit(json!({"event":"phase","phase":phase}))).await?)
                },
                None=>None,
            };
            let evaluated=execute_steps(&plan.evaluate.entries,&plugins,&mut values,&context,&cancel,&log).await;
            // The report keeps the built-in evidence even when a plugin check failed.
            let code=match measured {
                Some(measured)=>{
                    log.emit(json!({"event":"phase","phase":"checks"}))?;
                    crate::measurement::report(&dir,measured,&plugin_checks(&plan,&plugins,&values)?)?
                },
                None=>0,
            };
            evaluated?;
            Ok::<_,anyhow::Error>(code)
        };
        let result=tokio::select! {
            biased;
            _=cancel.cancelled()=>Err(anyhow::anyhow!("run cancelled")),
            result=tokio::time::timeout(Duration::from_millis(plan.timeout_ms).saturating_sub(preparation_started.elapsed()),main)=>result.unwrap_or_else(|_|Err(anyhow::anyhow!("whole-run deadline exceeded"))),
        };
        log.emit(json!({"event":"phase","phase":"teardown"}))?;
        let cleanup=tokio::time::timeout(Duration::from_secs(30),cleanup_steps(&plan.teardown.entries,&plugins,&mut values,&context,&log)).await;
        plugins.shutdown(&context).await;
        let cleanup=cleanup.map_err(|_|anyhow::anyhow!("teardown deadline exceeded")).and_then(|r|r);
        if let Err(error)=&cleanup {log.emit(json!({"event":"cleanup-failed","error":error.to_string()}))?;}
        match (result,cleanup) {(Ok(code),Ok(()))=>Ok(code),(Err(error),_)|(_,Err(error))=>Err(error)}
    }.await;
	let (exit_code, state, mut error) = match result {
		Ok(0) => (0, "completed", None),
		Ok(code) => (code, "completed_with_failures", None),
		Err(error) => (
			if cancel.is_cancelled() { 130 } else { 1 },
			if cancel.is_cancelled() { "stopped" } else { "failed" },
			Some(error.to_string()),
		),
	};
	if let Some(error) = error.as_mut() {
		for secret in log.secrets.iter() {
			*error = error.replace(secret, "[redacted]");
		}
	}
	let outcome = Outcome {
		artifact_dir: context.artifact_dir.clone(),
		exit_code,
		state: state.into(),
		error,
	};
	std::fs::write(directory.join("execution.json"), serde_json::to_vec_pretty(&outcome)?)?;
	if !directory.join("summary.md").exists() {
		std::fs::write(
			directory.join("summary.md"),
			format!(
				"# {}\n\nStatus: {}\n\n{}\n",
				plan.name,
				outcome.state,
				outcome.error.as_deref().unwrap_or("All configured steps completed.")
			),
		)?;
	}
	log.emit(json!({"event":"completed","outcome":outcome}))?;
	crate::artifacts::write_samples(&directory)?;
	Ok(outcome)
}
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
		log.emit(json!({"event":"step-started","step":step.id,"operation":step.operation,"user":context.user,"iteration":context.iteration}))?;
		let result = {
			let pending = invoke(step, plugins, values, context, cancel);
			tokio::pin!(pending);
			let mut progress = tokio::time::interval(Duration::from_secs(5));
			progress.tick().await;
			loop {
				tokio::select! {
					result=&mut pending => break result,
					_=progress.tick()=>log.emit(json!({"event":"step-progress","step":step.id,"elapsedMs":started.elapsed().as_millis()}))?,
				}
			}
		};
		log.emit(json!({"event":"step-finished","step":step.id,"user":context.user,"iteration":context.iteration,"elapsedMs":started.elapsed().as_millis(),"success":result.is_ok(),"error":result.as_ref().err().map(ToString::to_string)}))?;
		values.insert(format!("steps.{}", step.id), result?);
	}
	Ok(())
}
async fn invoke(
	step: &Step,
	plugins: &Plugins,
	values: &Values,
	context: &Context,
	cancel: &CancellationToken,
) -> Result<Value> {
	let inputs: serde_json::Map<String, Value> = step
		.inputs
		.iter()
		.map(|i| {
			Ok((
				i.name.clone(),
				if let Some(reference) = &i.reference {
					resolve(values, reference)?
				} else {
					i.literal()?
				},
			))
		})
		.collect::<Result<_>>()?;
	let inputs = Value::Object(inputs);
	let operation = contract(plugins, &step.operation)?;
	operation.validate_inputs(&inputs)?;
	if !step.operation.starts_with("core.") {
		return plugins.invoke(&step.operation, inputs, context, step.timeout_ms, cancel).await;
	}
	let work = async {
		match step.operation.as_str() {
			"core.echo" => Ok(json!({"value":inputs["value"]})),
			"core.assert-equal" => {
				ensure!(inputs["actual"] == inputs["expected"], "assertion {} failed", step.id);
				Ok(json!({"passed":true}))
			},
			"core.submit-prepared" | "core.submit-setup" => {
				submit(&inputs, context, step.operation == "core.submit-setup").await
			},
			_ => anyhow::bail!("unknown built-in"),
		}
	};
	tokio::select! {biased;_=cancel.cancelled()=>anyhow::bail!("run cancelled"),result=tokio::time::timeout(Duration::from_millis(step.timeout_ms),work)=>result.map_err(|_|anyhow::anyhow!("step {} timed out; submission outcome may be unknown",step.id))?}
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
		ensure!(
			hash.trim_start_matches("0x") == tx.hash.trim_start_matches("0x"),
			"RPC returned unexpected transaction hash"
		);
		hashes.push(hash);
		if wait_finalized {
			loop {
				let block = blocks.next().await.context("finalized block stream ended")??;
				let body = client.body(block.hash().0).await?;
				let Some(index) = body.iter().position(|bytes| {
					hex::encode(polkameter_chain::tx_hash(bytes))
						== tx.hash.trim_start_matches("0x")
				}) else {
					continue;
				};
				let events = polkameter_chain::events(&client.at(block.hash().0).await?).await?;
				ensure!(
					!events.iter().any(|e| e.2 == Some(index as u32)
						&& e.0 == "System" && e.1 == "ExtrinsicFailed"),
					"setup extrinsic failed"
				);
				if tx.metadata.get("sudo") == Some(&Value::Bool(true)) {
					ensure!(
						events.iter().any(|e| e.2 == Some(index as u32)
							&& e.0 == "Sudo" && e.1 == "Sudid"
							&& polkameter_chain::value::nth(&e.3, 0)
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
	Ok(json!({"hashes":hashes,"at":finalized_at}))
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
		let checks: Vec<polkameter_checks::CheckResult> =
			serde_json::from_value(value["checks"].clone())
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
				let target = plan
					.targets
					.entries
					.iter()
					.find(|target| target.id == requirement.target)
					.context("required RPC target missing")?;
				let client = polkameter_chain::Client::connect(&target.endpoint).await?;
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
				let monitor =
					plan.monitors.as_ref().context("metric requirement needs a topology")?;
				let targets = polkameter_monitors::load_targets(
					registry.topologies.get(&monitor.topology).context("topology missing")?,
					monitor.para_id,
				)?;
				polkameter_monitors::preflight::require_metrics(
					&targets,
					&[(requirement.target.clone(), requirement.name.clone())],
				)
				.await?;
			},
			other => anyhow::bail!("unsupported required evidence kind {other}"),
		}
	}
	Ok(())
}

async fn cleanup_steps(
	steps: &[Step],
	plugins: &Plugins,
	values: &mut Values,
	context: &Context,
	log: &Log,
) -> Result<()> {
	let mut first = None;
	for step in steps {
		if let Err(error) = execute_steps(
			std::slice::from_ref(step),
			plugins,
			values,
			context,
			&CancellationToken::new(),
			log,
		)
		.await
		{
			first.get_or_insert(error);
		}
	}
	match first {
		Some(error) => Err(error),
		None => Ok(()),
	}
}

fn validate_interval(configured: f64, measured: f64) -> Result<()> {
	anyhow::ensure!(
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
}
