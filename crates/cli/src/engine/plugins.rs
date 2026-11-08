use crate::engine::plan::{Credential, PlanError, PluginSpec};
use crate::redact::{Redactor, StreamRedactor};
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
	io::{AsyncReadExt as _, AsyncWriteExt, BufReader},
	process::{Child, ChildStderr, ChildStdin, ChildStdout, Command},
	sync::Mutex,
	task::JoinHandle,
};
use tokio_util::sync::CancellationToken;

/// The plugin registry file. Unknown fields, such as the `capabilities` that PolkaMeter 0.3
/// wrote, are ignored and dropped on the next write.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Registry {
	#[serde(default)]
	pub plugins: BTreeMap<String, Installation>,
	/// Credential profile to local environment variable. No secret values are persisted.
	#[serde(default)]
	pub credentials: BTreeMap<String, String>,
	#[serde(default)]
	pub topologies: BTreeMap<String, PathBuf>,
	/// The file this registry was read from, for error messages. Not stored.
	#[serde(skip)]
	pub source: Option<PathBuf>,
	/// The credential profile whose variable `--credential-env` set for this command, so that
	/// its errors name the flag. Not stored.
	#[serde(skip)]
	pub flag_profile: Option<String>,
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
		let mut registry: Self = match std::fs::read(path) {
			Ok(bytes) => serde_json::from_slice(&bytes).map_err(|e| {
				anyhow::anyhow!(
					"plugin registry {shown} is not a valid registry: {e}. Fix the file, or move \
					 it aside and run `polkameter plugin install` (and `plugin credential` and \
					 `plugin topology`) again"
				)
			})?,
			Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
			Err(e) => anyhow::bail!("cannot read plugin registry {shown}: {e}"),
		};
		registry.source = Some(path.to_owned());
		Ok(registry)
	}
	/// The file a registry path stands for: a symbolic link is followed to its target, even a
	/// target that does not exist yet, so that the registry is written through the link and the
	/// lock and temporary files sit next to the real file.
	pub fn resolve(path: &Path) -> Result<PathBuf> {
		let mut resolved = path.to_owned();
		// As many links as the kernel follows (Linux allows 40).
		for _ in 0..40 {
			match std::fs::symlink_metadata(&resolved) {
				Ok(metadata) if metadata.file_type().is_symlink() => {
					let target = std::fs::read_link(&resolved).with_context(|| {
						format!("cannot read plugin registry link {}", resolved.display())
					})?;
					resolved = match resolved.parent() {
						Some(parent) => parent.join(target),
						None => target,
					};
				},
				_ => return Ok(resolved),
			}
		}
		anyhow::bail!(
			"plugin registry {} is a chain of more than 40 symbolic links; point \
			 POLKAMETER_PLUGIN_REGISTRY at the registry file",
			path.display()
		)
	}
	/// Takes the exclusive lock for the registry at `path`, waiting for other holders. The lock
	/// is a separate `<registry>.lock` file next to it, because the registry itself is replaced
	/// on every write. Pass the [`Registry::resolve`]d path. When another process holds the lock,
	/// a line on standard error says what this one is waiting for.
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
		match file.try_lock() {
			Ok(()) => {},
			Err(std::fs::TryLockError::WouldBlock) => {
				eprintln!(
					"{}: waiting for {} (another polkameter is updating the registry)",
					crate::PRODUCT_NAME,
					lock_path.display()
				);
				file.lock().with_context(context)?;
			},
			Err(std::fs::TryLockError::Error(error)) => {
				return Err(anyhow::Error::new(error).context(context()));
			},
		}
		Ok(RegistryLock { _file: file })
	}
	/// Replaces the registry at `path` atomically: `<registry>.tmp` in the same directory is
	/// written, flushed and renamed over it. Call it while holding [`Registry::lock`], with the
	/// [`Registry::resolve`]d path, so that no other writer uses the temporary name.
	///
	/// The temporary file is created exclusively (never through a planted file or link) and is
	/// readable only by its owner until it takes the registry's permissions. A leftover one from a
	/// crashed writer is removed first; removing a link removes the link, not its target.
	pub fn write(&self, path: &Path) -> Result<()> {
		use std::io::Write as _;
		let context = || format!("cannot write plugin registry {}", path.display());
		if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
			std::fs::create_dir_all(parent).with_context(context)?;
		}
		let mut name = path.as_os_str().to_owned();
		name.push(".tmp");
		let temporary = PathBuf::from(name);
		match std::fs::remove_file(&temporary) {
			Err(error) if error.kind() != std::io::ErrorKind::NotFound => {
				return Err(error).with_context(context);
			},
			_ => {},
		}
		let mut options = std::fs::OpenOptions::new();
		options.write(true).create_new(true);
		#[cfg(unix)]
		std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
		let mut file = options.open(&temporary).with_context(context)?;
		let result = (|| -> std::io::Result<()> {
			file.write_all(&serde_json::to_vec_pretty(self)?)?;
			// The registry keeps its permissions; a new registry stays owner-only.
			if let Ok(existing) = std::fs::metadata(path) {
				file.set_permissions(existing.permissions())?;
			}
			file.sync_all()?;
			drop(file);
			rename_over(&temporary, path)
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
	/// Checks that plugin `id` is installed as `version` and that its executable has not changed
	/// since `polkameter plugin install`. Each message says how to fix it before the evidence.
	pub fn verify(&self, id: &str, version: &str) -> Result<&Installation> {
		let installed = self.plugins.get(id).with_context(|| {
			let registry = self
				.source
				.as_ref()
				.map_or_else(String::new, |path| format!(" (registry {})", path.display()));
			format!(
				"plugin {id} is not installed on this host; run `polkameter plugin install \
				 <path-to-executable>`{registry}"
			)
		})?;
		let path = installed.executable.display();
		ensure!(
			installed.version == version,
			"plugin {id} version mismatch: set version=\"{}\" in the plan's <plugin>, or install \
			 the build the plan expects with `polkameter plugin install <path-to-executable>`. The \
			 plan expects version {version}, but version {} is installed from {path}",
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
			"plugin {id} checksum mismatch: if you rebuilt it, run `polkameter plugin install \
			 {path}` again. {path} changed after it was installed (BLAKE2 expected {}, found \
			 {actual})",
			installed.blake2
		);
		Ok(installed)
	}
	/// The environment variable that holds `credential`'s secret, and whether `--credential-env`
	/// named it. The error names the credential, its profile and how to map it.
	fn credential_variable(&self, credential: &Credential) -> Result<(&str, bool)> {
		let (id, profile) = (&credential.id, &credential.profile);
		let variable = self.credentials.get(profile).ok_or_else(|| {
			let registry = self
				.source
				.as_ref()
				.map_or_else(String::new, |path| format!(" (registry {})", path.display()));
			PlanError::at(
				credential.location,
				format!(
					"credential `{id}` uses profile `{profile}`, which is not configured on this \
					 host; run `polkameter plugin credential {profile} <ENV_VAR>` to name the \
					 environment variable that holds it{registry}"
				),
			)
		})?;
		Ok((variable, self.flag_profile.as_deref() == Some(profile)))
	}
	/// The value of `credential`'s variable. Errors name the credential, its profile and the
	/// variable, never a value, and say how to fix it.
	fn credential_value(&self, credential: &Credential) -> Result<String> {
		let (id, profile) = (&credential.id, &credential.profile);
		let (variable, from_flag) = self.credential_variable(credential)?;
		let problem = match std::env::var(variable) {
			Ok(value) if !value.is_empty() => return Ok(value),
			Ok(_) => "is empty",
			Err(std::env::VarError::NotPresent) => "is not set",
			Err(std::env::VarError::NotUnicode(_)) => "is not valid Unicode",
		};
		let (source, fix) = if from_flag {
			(
				"from --credential-env",
				"pass the variable that holds it to --credential-env".to_owned(),
			)
		} else {
			(
				"mapped by `polkameter plugin credential`",
				format!(
					"map the profile to another variable with `polkameter plugin credential \
					 {profile} <ENV_VAR>`"
				),
			)
		};
		Err(PlanError::at(
			credential.location,
			format!(
				"credential `{id}` (profile `{profile}`) reads environment variable {variable} \
				 ({source}), which {problem}; set {variable} on the step that runs polkameter, or \
				 {fix}"
			),
		))
	}
	/// Checks every credential the plan declares: its profile is mapped, and the variable is set
	/// and not empty. No value leaves this function.
	pub fn check_credentials(&self, plan: &crate::engine::plan::Plan) -> Result<()> {
		for credential in &plan.credentials.entries {
			self.credential_value(credential)?;
		}
		Ok(())
	}
	pub fn resolve_credentials(
		&self,
		plan: &crate::engine::plan::Plan,
	) -> Result<BTreeMap<String, Value>> {
		plan.credentials
			.entries
			.iter()
			.map(|c| {
				Ok((format!("credentials.{}", c.id), Value::String(self.credential_value(c)?)))
			})
			.collect()
	}
}
/// Renames `from` over `to`. On Windows a reader that has the registry open makes the rename
/// fail with "access denied" for a moment, so it is retried briefly.
fn rename_over(from: &Path, to: &Path) -> std::io::Result<()> {
	let mut attempts = 0;
	loop {
		match std::fs::rename(from, to) {
			Err(error)
				if cfg!(windows)
					&& error.kind() == std::io::ErrorKind::PermissionDenied
					&& attempts < 20 =>
			{
				attempts += 1;
				std::thread::sleep(Duration::from_millis(50));
			},
			result => return result,
		}
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

/// Serializes writes to plugin logs with the passes that rewrite them, so a rewrite never drops
/// a line that was streamed meanwhile. The guard is held for one write, or for the end of one
/// rewrite: never across an `.await`.
pub(crate) fn lock_logs() -> std::sync::MutexGuard<'static, ()> {
	static WRITES: std::sync::Mutex<()> = std::sync::Mutex::new(());
	WRITES.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Opens a plugin log to append to it, readable only by its owner.
fn open_log(path: &Path) -> std::io::Result<std::fs::File> {
	let mut options = std::fs::OpenOptions::new();
	options.create(true).append(true);
	#[cfg(unix)]
	{
		use std::os::unix::fs::OpenOptionsExt;
		options.mode(0o600);
	}
	options.open(path)
}

/// Copies a plugin's standard error into its log at `path`, redacting it as it streams, so a
/// complete secret never reaches the file, even if the run is killed. Ends when every process
/// holding the pipe has exited. The log is opened for each write, so a rewrite pass can replace
/// it. Bytes held back because they may start a secret are written as `[redacted]` if the
/// stream ends there; if PolkaMeter itself dies they are lost, never written.
fn stream_log(mut stderr: ChildStderr, path: PathBuf, redactor: Redactor) -> JoinHandle<()> {
	use std::io::Write as _;
	let write = move |bytes: &[u8]| {
		if !bytes.is_empty() {
			let _writes = lock_logs();
			if let Ok(mut file) = open_log(&path) {
				let _ = file.write_all(bytes);
			}
		}
	};
	tokio::spawn(async move {
		let mut stream = StreamRedactor::new(redactor);
		let mut buffer = vec![0; 8192];
		while let Ok(read @ 1..) = stderr.read(&mut buffer).await {
			write(&stream.push(&buffer[..read]));
		}
		write(&stream.finish());
	})
}

/// Where a plugin's standard error goes: a log, redacted as it streams, or nowhere.
type Stderr<'a> = Option<(&'a Path, &'a Redactor)>;

struct Process {
	child: Child,
	group: Group,
	/// The task that copies standard error into the log.
	log: Option<JoinHandle<()>>,
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
	pub fn spawn(executable: &Path, context: &Context, stderr: Stderr<'_>) -> Result<Self> {
		let mut command = Command::new(executable);
		command.env_clear();
		for key in ["PATH", "TMPDIR", "TEMP", "SYSTEMROOT", "SSL_CERT_FILE", "SSL_CERT_DIR"] {
			if let Some(value) = std::env::var_os(key) {
				command.env(key, value);
			}
		}
		let log = if let Some((path, redactor)) = stderr {
			// Created now, so the log exists even if the plugin never writes to it.
			open_log(path)?;
			Some((path.to_owned(), redactor.clone()))
		} else {
			None
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
			.stderr(if log.is_some() { Stdio::piped() } else { Stdio::null() })
			.kill_on_drop(true)
			.spawn()?;
		let log = match (log, child.stderr.take()) {
			(Some((path, redactor)), Some(stderr)) => Some(stream_log(stderr, path, redactor)),
			_ => None,
		};
		let input =
			child.stdin.take().ok_or_else(|| anyhow::anyhow!("plugin stdin unavailable"))?;
		let output = BufReader::new(
			child
				.stdout
				.take()
				.ok_or_else(|| anyhow::anyhow!("plugin stdout unavailable"))?,
		);
		let group = Group(child.id());
		Ok(Self { child, group, log, input, output, sequence: 0, failed: false })
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
	/// so nothing it started outlives the run, and for the rest of its standard error to reach
	/// the log.
	async fn reap(&mut self) {
		self.group.kill();
		let _ = self.child.start_kill();
		if let Ok(Ok(_)) = tokio::time::timeout(Duration::from_secs(2), self.child.wait()).await {
			self.group.forget();
		}
		if let Some(mut log) = self.log.take() {
			// A process that left the group may still hold the pipe; stop copying from it.
			if tokio::time::timeout(Duration::from_secs(2), &mut log).await.is_err() {
				log.abort();
			}
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

/// Where a restarted plugin process writes its standard error: the same log as the first one,
/// redacted the same way.
struct Launch {
	stderr: PathBuf,
	redactor: Option<Redactor>,
}
impl Launch {
	fn stderr(&self) -> Stderr<'_> {
		self.redactor.as_ref().map(|redactor| (self.stderr.as_path(), redactor))
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
	launches: BTreeMap<String, Launch>,
}
impl std::ops::Deref for Plugins {
	type Target = PluginSet;
	fn deref(&self) -> &PluginSet {
		&self.0
	}
}
impl Plugins {
	/// Starts the plugins of `specs`. With a redactor, each plugin's standard error goes to
	/// `plugins/<id>/stderr.log` in the run directory, redacted as it streams; without one it
	/// is discarded.
	pub async fn start(
		specs: &[PluginSpec],
		registry: &Registry,
		context: &Context,
		log: Option<&Redactor>,
	) -> Result<Self> {
		// Validate every installation before launching any plugin.
		let installed = specs
			.iter()
			.map(|spec| -> Result<_> {
				let installed = registry
					.verify(&spec.id, &spec.version)
					.map_err(|e| PlanError::at(spec.location, format!("{e:#}")))?;
				Ok((spec, installed))
			})
			.collect::<Result<Vec<_>>>()?;
		let mut set = PluginSet::default();
		for (spec, installed) in installed {
			let directory = context.artifact_dir.join("plugins").join(&spec.id);
			std::fs::create_dir_all(&directory)?;
			let launch = Launch { stderr: directory.join("stderr.log"), redactor: log.cloned() };
			let mut process = Process::spawn(&installed.executable, context, launch.stderr())?;
			let manifest = process.describe(context).await?;
			if !(manifest.id == spec.id
				&& manifest.version == spec.version
				&& manifest.protocol == spec.protocol)
			{
				let msg = format!(
					"plugin {}: run `polkameter plugin install {}` again. It answered $describe \
					 as {} {} (protocol {}), but the plan expects {} {} (protocol {})",
					spec.id,
					installed.executable.display(),
					manifest.id,
					manifest.version,
					manifest.protocol,
					spec.id,
					spec.version,
					spec.protocol,
				);
				return Err(PlanError::at(spec.location, msg));
			}
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
		let mut fresh = Process::spawn(&installed.executable, context, launch.stderr())?;
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

#[cfg(test)]
mod registry_tests {
	use super::*;

	/// A temporary directory, removed when dropped.
	struct Scratch(PathBuf);
	impl Scratch {
		fn new(name: &str) -> Self {
			let nanos = std::time::SystemTime::now()
				.duration_since(std::time::UNIX_EPOCH)
				.unwrap()
				.as_nanos();
			let path = std::env::temp_dir()
				.join(format!("polkameter-registry-{name}-{}-{nanos}", std::process::id()));
			std::fs::create_dir_all(&path).unwrap();
			Self(path.canonicalize().unwrap())
		}
	}
	impl Drop for Scratch {
		fn drop(&mut self) {
			let _ = std::fs::remove_dir_all(&self.0);
		}
	}

	fn registry_with(profile: &str) -> Registry {
		let mut registry = Registry::default();
		registry.credentials.insert(profile.into(), "VARIABLE".into());
		registry
	}

	#[cfg(unix)]
	#[test]
	fn a_new_registry_is_owner_only_and_a_rewrite_keeps_the_mode() {
		use std::os::unix::fs::PermissionsExt as _;
		let scratch = Scratch::new("mode");
		let path = scratch.0.join("plugins.json");
		let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
		registry_with("a").write(&path).unwrap();
		assert_eq!(mode(&path), 0o600);
		std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o640)).unwrap();
		registry_with("b").write(&path).unwrap();
		assert_eq!(mode(&path), 0o640);
		assert!(Registry::read(&path).unwrap().credentials.contains_key("b"));
	}
}
