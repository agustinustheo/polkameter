use crate::plan::PluginSpec;
use anyhow::{Context as _, Result, ensure};
use polkameter_plugin_sdk::{
	Context, MAX_MESSAGE_BYTES, Manifest, Operation, PROTOCOL, Request, Response,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
	collections::BTreeMap,
	path::{Path, PathBuf},
	process::Stdio,
	sync::Arc,
	time::Duration,
};
use tokio::{
	io::{AsyncWriteExt, BufReader},
	process::{Child, ChildStdin, ChildStdout, Command},
	sync::Mutex,
};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Registry {
	#[serde(default)]
	pub plugins: BTreeMap<String, Installation>,
	/// Credential profile to local environment variable. No secret values are persisted.
	#[serde(default)]
	pub credentials: BTreeMap<String, String>,
	#[serde(default)]
	pub topologies: BTreeMap<String, PathBuf>,
	#[serde(default)]
	pub capabilities: Vec<String>,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Installation {
	pub executable: PathBuf,
	pub version: String,
	pub blake2: String,
}
impl Registry {
	pub fn path() -> Result<PathBuf> {
		if let Some(path) = std::env::var_os("POLKAMETER_PLUGIN_REGISTRY") {
			return Ok(path.into());
		}
		Ok(PathBuf::from(
			std::env::var_os("HOME")
				.or_else(|| std::env::var_os("USERPROFILE"))
				.ok_or_else(|| anyhow::anyhow!("set POLKAMETER_PLUGIN_REGISTRY"))?,
		)
		.join(".config/polkameter/plugins.json"))
	}
	pub fn load() -> Result<Self> {
		Self::read(&Self::path()?)
	}
	pub fn read(path: &Path) -> Result<Self> {
		match std::fs::read(path) {
			Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
			Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
			Err(e) => Err(e.into()),
		}
	}
	pub fn write(&self, path: &Path) -> Result<()> {
		if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
			std::fs::create_dir_all(parent)?;
		}
		let temporary = path.with_extension(format!("{}.tmp", std::process::id()));
		std::fs::write(&temporary, serde_json::to_vec_pretty(self)?)?;
		std::fs::rename(temporary, path)?;
		Ok(())
	}
	pub async fn install(&mut self, executable: &Path) -> Result<Manifest> {
		let executable = executable.canonicalize()?;
		let context = Context::host("inspect", std::env::temp_dir());
		let mut process = Process::spawn(&executable, &context, None)?;
		let manifest = process.describe(&context).await?;
		process.shutdown(&context).await;
		ensure!(
			manifest.protocol == PROTOCOL && !manifest.id.is_empty() && manifest.id != "core",
			"invalid plugin identity"
		);
		self.plugins.insert(
			manifest.id.clone(),
			Installation {
				blake2: checksum(&executable)?,
				executable,
				version: manifest.version.clone(),
			},
		);
		Ok(manifest)
	}
	pub fn resolve_credentials(&self, plan: &crate::plan::Plan) -> Result<BTreeMap<String, Value>> {
		plan.credentials
			.entries
			.iter()
			.map(|c| {
				let variable = self.credentials.get(&c.profile).ok_or_else(|| {
					anyhow::anyhow!(
						"credential profile {} is not configured on this host",
						c.profile
					)
				})?;
				let value = std::env::var(variable)
					.with_context(|| format!("credential profile {} is unavailable", c.profile))?;
				ensure!(!value.is_empty(), "empty credential profile {}", c.profile);
				Ok((format!("credentials.{}", c.id), Value::String(value)))
			})
			.collect()
	}
}
pub(crate) fn checksum(path: &Path) -> Result<String> {
	Ok(polkameter_plugin_sdk::blake2_hex(&std::fs::read(path)?))
}

struct InFlight<'a> {
	child: &'a mut Child,
	failed: &'a mut bool,
}
impl Drop for InFlight<'_> {
	fn drop(&mut self) {
		if *self.failed {
			let _ = self.child.start_kill();
		}
	}
}

struct Process {
	child: Child,
	input: ChildStdin,
	output: BufReader<ChildStdout>,
	sequence: u64,
	failed: bool,
}
impl Process {
	pub fn spawn(executable: &Path, context: &Context, stderr: Option<&Path>) -> Result<Self> {
		let mut command = Command::new(executable);
		command.env_clear();
		for key in ["PATH", "TMPDIR", "TEMP", "SYSTEMROOT", "SSL_CERT_FILE", "SSL_CERT_DIR"] {
			if let Some(value) = std::env::var_os(key) {
				command.env(key, value);
			}
		}
		let stderr = if let Some(path) = stderr {
			let mut options = std::fs::OpenOptions::new();
			options.create(true).append(true);
			#[cfg(unix)]
			{
				use std::os::unix::fs::OpenOptionsExt;
				options.mode(0o600);
			}
			Stdio::from(options.open(path)?)
		} else {
			Stdio::null()
		};
		let mut child = command
			.current_dir(&context.artifact_dir)
			.stdin(Stdio::piped())
			.stdout(Stdio::piped())
			.stderr(stderr)
			.kill_on_drop(true)
			.spawn()?;
		let input =
			child.stdin.take().ok_or_else(|| anyhow::anyhow!("plugin stdin unavailable"))?;
		let output = BufReader::new(
			child
				.stdout
				.take()
				.ok_or_else(|| anyhow::anyhow!("plugin stdout unavailable"))?,
		);
		Ok(Self { child, input, output, sequence: 0, failed: false })
	}
	pub async fn describe(&mut self, context: &Context) -> Result<Manifest> {
		let value = self
			.call("$describe", serde_json::json!({}), context, 10_000, &CancellationToken::new())
			.await?;
		Ok(serde_json::from_value(value)?)
	}
	pub async fn call(
		&mut self,
		operation: &str,
		inputs: Value,
		context: &Context,
		timeout_ms: u64,
		cancel: &CancellationToken,
	) -> Result<Value> {
		ensure!(!self.failed, "plugin process failed; invocation is not retried");
		self.sequence += 1;
		let id = self.sequence;
		let request = Request {
			protocol: PROTOCOL,
			id,
			operation: operation.into(),
			inputs,
			context: context.clone(),
		};
		let bytes = serde_json::to_vec(&request)?;
		ensure!(bytes.len() < MAX_MESSAGE_BYTES, "request exceeds protocol limit");
		// Dropping this future (whole-run timeout/cancellation) must also kill
		// the in-flight process; no stale reply may be consumed by teardown.
		self.failed = true;
		let guard = InFlight { child: &mut self.child, failed: &mut self.failed };
		let exchange = async {
			self.input.write_all(&bytes).await?;
			self.input.write_all(b"\n").await?;
			self.input.flush().await?;
			let line = polkameter_plugin_sdk::read_line(&mut self.output)
				.await?
				.ok_or_else(|| anyhow::anyhow!("plugin exited before responding"))?;
			let response: Response = serde_json::from_str(&line)?;
			response.into_result(id)
		};
		let result = tokio::select! {
			biased;
			_ = cancel.cancelled() => Err(anyhow::anyhow!("run cancelled")),
			result = tokio::time::timeout(Duration::from_millis(timeout_ms), exchange) => result.unwrap_or_else(|_| Err(anyhow::anyhow!("plugin deadline exceeded"))),
		};
		*guard.failed = result.is_err();
		drop(guard);
		if result.is_err() {
			let _ = tokio::time::timeout(Duration::from_secs(2), self.child.wait()).await;
		}
		result
	}
	pub async fn shutdown(&mut self, context: &Context) {
		if !self.failed {
			let _ = self
				.call("$shutdown", serde_json::json!({}), context, 1000, &CancellationToken::new())
				.await;
		}
		let _ = self.child.start_kill();
		let _ = tokio::time::timeout(Duration::from_secs(2), self.child.wait()).await;
	}
}
/// The plugin processes of one run, shared by every task of the run.
#[derive(Clone, Default)]
pub struct Plugins(Arc<PluginSet>);
/// Each plugin's manifest and its serialized process, by plugin ID.
#[derive(Default)]
pub struct PluginSet {
	pub manifests: BTreeMap<String, Manifest>,
	processes: BTreeMap<String, Mutex<Process>>,
}
impl std::ops::Deref for Plugins {
	type Target = PluginSet;
	fn deref(&self) -> &PluginSet {
		&self.0
	}
}
impl Plugins {
	pub async fn start(
		specs: &[PluginSpec],
		registry: &Registry,
		context: &Context,
	) -> Result<Self> {
		// Validate every installation before launching any plugin.
		let installed = specs
			.iter()
			.map(|spec| -> Result<_> {
				let installed = registry
					.plugins
					.get(&spec.id)
					.with_context(|| format!("plugin {} is not installed on this host", spec.id))?;
				ensure!(
					installed.version == spec.version
						&& checksum(&installed.executable)? == installed.blake2,
					"plugin {} version/checksum mismatch",
					spec.id
				);
				Ok((spec, installed))
			})
			.collect::<Result<Vec<_>>>()?;
		let mut set = PluginSet::default();
		for (spec, installed) in installed {
			let directory = context.artifact_dir.join("plugins").join(&spec.id);
			std::fs::create_dir_all(&directory)?;
			let mut process = Process::spawn(
				&installed.executable,
				context,
				Some(&directory.join("stderr.log")),
			)?;
			let manifest = process.describe(context).await?;
			ensure!(
				manifest.id == spec.id
					&& manifest.version == spec.version
					&& manifest.protocol == spec.protocol,
				"manifest identity mismatch for {}",
				spec.id
			);
			set.manifests.insert(spec.id.clone(), manifest);
			set.processes.insert(spec.id.clone(), Mutex::new(process));
		}
		Ok(Self(Arc::new(set)))
	}
	pub fn operation(&self, plugin: &str, operation: &str) -> Result<&Operation> {
		self.manifests
			.get(plugin)
			.and_then(|p| p.operations.get(operation))
			.ok_or_else(|| anyhow::anyhow!("unknown operation {plugin}.{operation}"))
	}
	pub async fn invoke(
		&self,
		name: &str,
		inputs: Value,
		context: &Context,
		timeout_ms: u64,
		cancel: &CancellationToken,
	) -> Result<Value> {
		let (plugin, operation) =
			name.split_once('.').ok_or_else(|| anyhow::anyhow!("invalid operation"))?;
		let contract = self.operation(plugin, operation)?;
		contract.validate_inputs(&inputs)?;
		let queue = self.processes.get(plugin).context("plugin process is missing")?;
		let started = std::time::Instant::now();
		let mut process = tokio::select! {
			biased;
			_ = cancel.cancelled() => anyhow::bail!("run cancelled"),
			result = tokio::time::timeout(Duration::from_millis(timeout_ms), queue.lock()) => {
				result.map_err(|_| anyhow::anyhow!("plugin queue deadline exceeded"))?
			},
		};
		let remaining =
			timeout_ms.saturating_sub(started.elapsed().as_millis().try_into().unwrap_or(u64::MAX));
		ensure!(remaining > 0, "plugin queue deadline exceeded");
		let mut context = context.clone();
		context.artifact_dir = context.artifact_dir.join("plugins").join(plugin);
		std::fs::create_dir_all(&context.artifact_dir)?;
		let result = process.call(operation, inputs, &context, remaining, cancel).await?;
		contract.validate_outputs(&result)?;
		Ok(result)
	}
	pub async fn shutdown(&self, context: &Context) {
		for process in self.processes.values() {
			process.lock().await.shutdown(context).await;
		}
	}
}
