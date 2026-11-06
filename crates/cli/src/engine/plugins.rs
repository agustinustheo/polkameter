use crate::engine::plan::PluginSpec;
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
/// An exclusive lock on a registry file, held through a read-modify-write so that concurrent
/// `polkameter plugin` commands do not lose each other's changes. Released on drop.
#[derive(Debug)]
pub struct RegistryLock {
	_file: std::fs::File,
}

impl Registry {
	pub fn path() -> Result<PathBuf> {
		if let Some(path) = std::env::var_os("POLKAMETER_PLUGIN_REGISTRY") {
			return Ok(path.into());
		}
		Ok(PathBuf::from(
			std::env::var_os("HOME")
				.or_else(|| std::env::var_os("USERPROFILE"))
				.ok_or_else(|| {
					anyhow::anyhow!(
						"cannot find the plugin registry: neither HOME nor USERPROFILE is set; \
						 set POLKAMETER_PLUGIN_REGISTRY to the registry file's path"
					)
				})?,
		)
		.join(".config/polkameter/plugins.json"))
	}
	pub fn load() -> Result<Self> {
		Self::read(&Self::path()?)
	}
	/// Reads the registry at `path`; a missing file is an empty registry. An unreadable or
	/// corrupt file is an error that names the file and how to recover.
	pub fn read(path: &Path) -> Result<Self> {
		let shown = path.display();
		match std::fs::read(path) {
			Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| {
				anyhow::anyhow!(
					"plugin registry {shown} is not a valid registry: {e}. Fix the file, or move \
					 it aside and run `polkameter plugin install` (and `plugin credential`, \
					 `plugin topology`, `plugin capability`) again"
				)
			}),
			Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
			Err(e) => Err(anyhow::anyhow!("cannot read plugin registry {shown}: {e}")),
		}
	}
	/// Takes the exclusive lock for the registry at `path`, waiting for other holders. The lock
	/// is a separate `<registry>.lock` file next to it, because the registry itself is replaced
	/// on every write.
	pub fn lock(path: &Path) -> Result<RegistryLock> {
		let mut name = path.as_os_str().to_owned();
		name.push(".lock");
		let lock_path = PathBuf::from(name);
		let context = || format!("cannot lock plugin registry {}", lock_path.display());
		if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
			std::fs::create_dir_all(parent).with_context(context)?;
		}
		let file = std::fs::OpenOptions::new()
			.create(true)
			.truncate(false)
			.write(true)
			.open(&lock_path)
			.with_context(context)?;
		file.lock().with_context(context)?;
		Ok(RegistryLock { _file: file })
	}
	/// Replaces the registry at `path` atomically: a temporary file in the same directory is
	/// written, flushed and renamed over it. Call it while holding [`Registry::lock`].
	pub fn write(&self, path: &Path) -> Result<()> {
		use std::io::Write as _;
		static SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
		let context = || format!("cannot write plugin registry {}", path.display());
		if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
			std::fs::create_dir_all(parent).with_context(context)?;
		}
		let sequence = SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
		let mut name = path.file_name().unwrap_or_default().to_owned();
		name.push(format!(".{}.{sequence}.tmp", std::process::id()));
		let temporary = path.with_file_name(name);
		let result = (|| -> std::io::Result<()> {
			let mut file = std::fs::File::create(&temporary)?;
			file.write_all(&serde_json::to_vec_pretty(self)?)?;
			file.sync_all()?;
			std::fs::rename(&temporary, path)
		})();
		if result.is_err() {
			let _ = std::fs::remove_file(&temporary);
		}
		result.with_context(context)
	}
	/// Asks the executable for its manifest and pins its checksum, without changing a registry.
	pub async fn describe(executable: &Path) -> Result<(Manifest, Installation)> {
		let executable = executable
			.canonicalize()
			.with_context(|| format!("cannot read plugin executable {}", executable.display()))?;
		let context = Context::host("inspect", std::env::temp_dir());
		let mut process = Process::spawn(&executable, &context, None)
			.with_context(|| format!("cannot start plugin executable {}", executable.display()))?;
		let manifest = process.describe(&context).await.with_context(|| {
			format!("plugin executable {} did not answer $describe", executable.display())
		})?;
		process.shutdown(&context, &CancellationToken::new()).await;
		ensure!(
			manifest.protocol == PROTOCOL,
			"plugin {} speaks protocol {}, but this PolkaMeter speaks protocol {PROTOCOL}; \
			 rebuild the plugin against a matching polkameter-plugin-sdk",
			executable.display(),
			manifest.protocol
		);
		ensure!(
			!manifest.id.is_empty() && manifest.id != "core",
			"plugin {} reports the ID `{}`; a plugin ID must not be empty or `core`",
			executable.display(),
			manifest.id
		);
		let installation = Installation {
			blake2: checksum(&executable)?,
			executable,
			version: manifest.version.clone(),
		};
		Ok((manifest, installation))
	}
	pub async fn install(&mut self, executable: &Path) -> Result<Manifest> {
		let (manifest, installation) = Self::describe(executable).await?;
		self.plugins.insert(manifest.id.clone(), installation);
		Ok(manifest)
	}
	/// Checks that plugin `id` is installed as `version` and that its executable has not changed
	/// since `polkameter plugin install`.
	pub fn verify(&self, id: &str, version: &str) -> Result<&Installation> {
		let installed = self.plugins.get(id).with_context(|| {
			format!(
				"plugin {id} is not installed on this host; run `polkameter plugin install \
				 <path-to-executable>`"
			)
		})?;
		let path = installed.executable.display();
		ensure!(
			installed.version == version,
			"plugin {id} version mismatch: the plan expects version {version}, but version {} is \
			 installed from {path}. Set version=\"{}\" in the plan's <plugin>, or install the \
			 matching build with `polkameter plugin install <path-to-executable>`",
			installed.version,
			installed.version
		);
		let actual = checksum(&installed.executable).with_context(|| {
			format!(
				"cannot read plugin {id} executable {path}; reinstall it with `polkameter plugin \
				 install <path-to-executable>`"
			)
		})?;
		ensure!(
			actual == installed.blake2,
			"plugin {id} checksum mismatch: {path} changed after it was installed (BLAKE2 \
			 expected {}, found {actual}). If you rebuilt it, run `polkameter plugin install \
			 {path}` again",
			installed.blake2
		);
		Ok(installed)
	}
	pub fn resolve_credentials(
		&self,
		plan: &crate::engine::plan::Plan,
	) -> Result<BTreeMap<String, Value>> {
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
	group: &'a mut Group,
	failed: &'a mut bool,
}
impl Drop for InFlight<'_> {
	fn drop(&mut self) {
		if *self.failed {
			self.group.kill();
			let _ = self.child.start_kill();
		}
	}
}

/// The process group a plugin runs in, led by the plugin process. Plugins get their own group, so
/// a terminal's Ctrl-C or a signal to PolkaMeter's group does not reach them, and everything a
/// plugin starts is killed with it. On Windows the plugin's process tree is killed instead.
struct Group(Option<u32>);
impl Group {
	/// Kills every process in the group. Called before the leader is reaped, so its ID cannot have
	/// been reused.
	fn kill(&mut self) {
		let Some(id) = self.0 else { return };
		#[cfg(unix)]
		{
			use nix::{sys::signal, unistd::Pid};
			if let Ok(id) = i32::try_from(id) {
				let _ = signal::killpg(Pid::from_raw(id), signal::Signal::SIGKILL);
			}
		}
		#[cfg(windows)]
		{
			let _ = std::process::Command::new("taskkill")
				.args(["/T", "/F", "/PID", &id.to_string()])
				.stdout(Stdio::null())
				.stderr(Stdio::null())
				.status();
		}
		#[cfg(not(any(unix, windows)))]
		let _ = id;
	}
	/// The group is gone; never signal its ID again.
	fn forget(&mut self) {
		self.0 = None;
	}
}

struct Process {
	child: Child,
	group: Group,
	input: ChildStdin,
	output: BufReader<ChildStdout>,
	sequence: u64,
	/// Set when the process broke the protocol, crashed, timed out or was cancelled. A failed
	/// process is killed and takes no more calls until it is restarted.
	failed: bool,
}
/// A reply that followed the protocol: the operation's result, or the error it reported.
type Reply = std::result::Result<Value, String>;
/// Checks a reply's envelope. An `Err` is a protocol violation; an operation error is `Ok(Err)`.
fn reply(response: Response, id: u64) -> Result<Reply> {
	ensure!(
		response.protocol == PROTOCOL && response.id == id,
		"plugin protocol/request ID mismatch"
	);
	match (response.result, response.error) {
		(Some(result), None) => Ok(Ok(result)),
		(None, Some(error)) => Ok(Err(error)),
		_ => anyhow::bail!("invalid plugin response"),
	}
}
impl Drop for Process {
	fn drop(&mut self) {
		// A dropped run (forced exit) still takes the plugin's whole group down.
		self.group.kill();
	}
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
		#[cfg(unix)]
		command.process_group(0);
		#[cfg(windows)]
		{
			// CREATE_NEW_PROCESS_GROUP: a console Ctrl-C does not reach the plugin.
			command.creation_flags(0x0000_0200);
		}
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
		let group = Group(child.id());
		Ok(Self { child, group, input, output, sequence: 0, failed: false })
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
		let guard =
			InFlight { child: &mut self.child, group: &mut self.group, failed: &mut self.failed };
		let exchange = async {
			self.input.write_all(&bytes).await?;
			self.input.write_all(b"\n").await?;
			self.input.flush().await?;
			let line = polkameter_plugin_sdk::read_line(&mut self.output)
				.await?
				.ok_or_else(|| anyhow::anyhow!("plugin exited before responding"))?;
			reply(serde_json::from_str(&line)?, id)
		};
		let result = tokio::select! {
			biased;
			() = cancel.cancelled() => Err(anyhow::anyhow!("run cancelled")),
			result = tokio::time::timeout(Duration::from_millis(timeout_ms), exchange) => result.unwrap_or_else(|_| Err(anyhow::anyhow!("plugin deadline exceeded"))),
		};
		// Only a broken exchange ends the process. An operation error is a normal reply: the
		// plugin stays usable for later steps and teardown.
		*guard.failed = result.is_err();
		drop(guard);
		if result.is_err() {
			self.reap().await;
		}
		result?.map_err(|error| anyhow::anyhow!("plugin operation failed: {error}"))
	}
	/// Kills the plugin and everything in its group, then waits briefly for the plugin to exit,
	/// so nothing it started outlives the run.
	async fn reap(&mut self) {
		self.group.kill();
		let _ = self.child.start_kill();
		if let Ok(Ok(_)) = tokio::time::timeout(Duration::from_secs(2), self.child.wait()).await {
			self.group.forget();
		}
	}
	/// Whether the plugin can take another call: it did not fail and has not exited, for example
	/// because something outside PolkaMeter killed it while it was idle.
	fn usable(&mut self) -> bool {
		if self.failed {
			return false;
		}
		match self.child.try_wait() {
			Ok(None) => true,
			_ => {
				self.failed = true;
				// The leader exited; whatever it started may still run in its group.
				self.group.kill();
				self.group.forget();
				false
			},
		}
	}
	/// Asks the plugin to exit, unless `force` is set, then kills and reaps it.
	pub async fn shutdown(&mut self, context: &Context, force: &CancellationToken) {
		if !self.failed && !force.is_cancelled() {
			let _ = self.call("$shutdown", serde_json::json!({}), context, 1000, force).await;
		}
		self.reap().await;
	}
}

/// Where a restarted plugin process writes its standard error: the same log as the first one.
struct Launch {
	stderr: PathBuf,
}
/// The plugin processes of one run, shared by every task of the run.
#[derive(Clone, Default)]
pub struct Plugins(Arc<PluginSet>);
/// Each plugin's manifest and its serialized process, by plugin ID.
#[derive(Default)]
pub struct PluginSet {
	pub manifests: BTreeMap<String, Manifest>,
	processes: BTreeMap<String, Mutex<Process>>,
	launches: BTreeMap<String, Launch>,
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
			.map(|spec| -> Result<_> { Ok((spec, registry.verify(&spec.id, &spec.version)?)) })
			.collect::<Result<Vec<_>>>()?;
		let mut set = PluginSet::default();
		for (spec, installed) in installed {
			let directory = context.artifact_dir.join("plugins").join(&spec.id);
			std::fs::create_dir_all(&directory)?;
			let launch = Launch { stderr: directory.join("stderr.log") };
			let mut process = Process::spawn(&installed.executable, context, Some(&launch.stderr))?;
			let manifest = process.describe(context).await?;
			ensure!(
				manifest.id == spec.id
					&& manifest.version == spec.version
					&& manifest.protocol == spec.protocol,
				"plugin {} answered $describe as {} {} (protocol {}), but the plan expects {} {} \
				 (protocol {}); run `polkameter plugin install {}` again",
				spec.id,
				manifest.id,
				manifest.version,
				manifest.protocol,
				spec.id,
				spec.version,
				spec.protocol,
				installed.executable.display()
			);
			set.manifests.insert(spec.id.clone(), manifest);
			set.processes.insert(spec.id.clone(), Mutex::new(process));
			set.launches.insert(spec.id.clone(), launch);
		}
		Ok(Self(Arc::new(set)))
	}
	/// Starts `plugin` again if its process failed (crash, protocol violation, timeout or
	/// cancellation), re-checking its installation and manifest first. Returns whether it was
	/// restarted. A plugin that only returned operation errors is left running.
	pub async fn restart_failed(
		&self,
		plugin: &str,
		registry: &Registry,
		context: &Context,
	) -> Result<bool> {
		let (Some(queue), Some(launch), Some(manifest)) =
			(self.processes.get(plugin), self.launches.get(plugin), self.manifests.get(plugin))
		else {
			return Ok(false);
		};
		let mut process = queue.lock().await;
		if process.usable() {
			return Ok(false);
		}
		process.reap().await;
		let installed = registry.verify(plugin, &manifest.version)?;
		let mut fresh = Process::spawn(&installed.executable, context, Some(&launch.stderr))?;
		let described = match fresh.describe(context).await {
			Ok(described) => described,
			Err(error) => {
				fresh.reap().await;
				return Err(error);
			},
		};
		if serde_json::to_value(&described)? != serde_json::to_value(manifest)? {
			fresh.reap().await;
			anyhow::bail!("plugin {plugin} answered with a different manifest after a restart");
		}
		*process = fresh;
		Ok(true)
	}
	pub fn operation(&self, plugin: &str, operation: &str) -> Result<&Operation> {
		self.manifests
			.get(plugin)
			.and_then(|p| p.operations.get(operation))
			.ok_or_else(|| {
				let offered = self.manifests.get(plugin).map_or_else(
					|| "none".to_owned(),
					|manifest| {
						let names: Vec<_> =
							manifest.operations.keys().map(String::as_str).collect();
						names.join(", ")
					},
				);
				anyhow::anyhow!(
					"unknown operation {plugin}.{operation}: the installed plugin {plugin} offers \
					 {offered}; `polkameter plugin inspect <plan>` lists them"
				)
			})
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
	/// Shuts every plugin down; with `force` set, kills them without asking. Either way each
	/// process is reaped before this returns.
	pub async fn shutdown(&self, context: &Context, force: &CancellationToken) {
		for process in self.processes.values() {
			process.lock().await.shutdown(context, force).await;
		}
	}
}
