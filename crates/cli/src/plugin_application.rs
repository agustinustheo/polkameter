//! Frontend adapters for the shared XML plugin engine.
use crate::{
	PRODUCT_NAME,
	engine::{execute, plan::Plan, plugins::Registry},
};
use serde::Serialize;
use std::{
	path::{Path, PathBuf},
	sync::Arc,
};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Debug, Default, serde::Deserialize, Serialize)]
pub struct Status {
	pub id: String,
	pub state: String,
	pub phase: Option<String>,
	pub outcome: Option<execute::Outcome>,
	pub error: Option<String>,
}
#[derive(Default)]
pub struct State {
	pub status: Mutex<Status>,
	pub history: Mutex<std::collections::BTreeMap<String, Status>>,
	pub progress: std::sync::Mutex<Option<String>>,
	/// Stops the active run, if there is one.
	pub cancel: Mutex<Option<execute::Stop>>,
	/// The task of the latest run; it ends once the run's outcome is recorded.
	pub task: Mutex<Option<tokio::task::JoinHandle<()>>>,
	/// Keep this many finished `run-…` directories under the output root, deleting older
	/// finished ones before a new run starts. `None` keeps them all.
	pub keep_runs: Option<usize>,
	/// The run ID being admitted (its plan inspected) but not yet started.
	pub admitting: std::sync::Mutex<Option<String>>,
	/// Cancelled when the host is shutting down: admission stops and no run starts.
	pub closing: CancellationToken,
}

/// Why a run did not start, and whether asking again may succeed.
#[derive(Debug)]
pub struct StartError {
	/// What went wrong.
	pub message: String,
	/// The same request may succeed later (the host is busy admitting it or shutting down).
	pub retry: bool,
}

impl StartError {
	fn refused(message: impl ToString) -> Self {
		Self { message: message.to_string(), retry: false }
	}
}

/// Clears the admission mark however admission ends.
struct Admission<'a>(&'a State);
impl Drop for Admission<'_> {
	fn drop(&mut self) {
		if let Ok(mut admitting) = self.0.admitting.lock() {
			*admitting = None;
		}
	}
}

/// Parses a plan and loads this host's registry: the inputs of every plugin gate.
pub fn resolve(xml: &str) -> anyhow::Result<(Plan, Registry)> {
	Ok((Plan::parse(xml)?, Registry::load()?))
}

/// The `inspect` gate over plan XML, for the desktop app and the remote agent.
pub async fn inspect_xml(xml: &str) -> anyhow::Result<serde_json::Value> {
	let (plan, registry) = resolve(xml)?;
	execute::inspect(&plan, &registry).await
}

/// The `preflight` gate over plan XML, for the desktop app and the remote agent.
pub async fn preflight_xml(xml: &str) -> anyhow::Result<serde_json::Value> {
	let (plan, registry) = resolve(xml)?;
	execute::preflight(&plan, &registry).await
}

pub async fn start(
	xml: String,
	root: String,
	state: Arc<State>,
	sink: execute::EventSink,
) -> Result<Status, String> {
	start_with_options(xml, root, state, sink, execute::RunOptions::default()).await
}

/// Starts a run with an explicit exit policy on the host that writes its artifacts.
pub async fn start_with_options(
	xml: String,
	root: String,
	state: Arc<State>,
	sink: execute::EventSink,
	options: execute::RunOptions,
) -> Result<Status, String> {
	start_with_id(xml, root, state, sink, options, None)
		.await
		.map_err(|error| error.message)
}

/// Starts a run under `run_id` when one is given. Starting again with the ID of a run this host
/// already started returns that run instead of starting another, so a client whose start
/// request lost its response can repeat it. The plan is inspected without holding the status
/// lock, and admission ends early when the host starts shutting down.
pub async fn start_with_id(
	xml: String,
	root: String,
	state: Arc<State>,
	sink: execute::EventSink,
	options: execute::RunOptions,
	run_id: Option<String>,
) -> Result<Status, StartError> {
	if let Some(id) = &run_id
		&& let Some(known) = known_run(&state, id).await
	{
		return Ok(known);
	}
	let (plan, registry) = resolve(&xml).map_err(StartError::refused)?;
	start_plan(plan, registry, root, state, sink, options, run_id).await
}

/// [`start_with_id`] for a parsed plan and a registry.
pub async fn start_plan(
	plan: Plan,
	registry: Registry,
	root: String,
	state: Arc<State>,
	sink: execute::EventSink,
	options: execute::RunOptions,
	run_id: Option<String>,
) -> Result<Status, StartError> {
	if let Some(id) = &run_id
		&& let Some(known) = known_run(&state, id).await
	{
		return Ok(known);
	}
	let admission = {
		let status = state.status.lock().await;
		if status.state == "running" {
			return Err(StartError::refused("a plugin plan is already running"));
		}
		let mut admitting = state
			.admitting
			.lock()
			.map_err(|_| StartError::refused("admission lock poisoned"))?;
		if let Some(other) = admitting.as_ref() {
			// The same client repeating its start while the first request is still admitted.
			let retry = run_id.as_ref() == Some(other);
			let message = if retry {
				"this run is still being started; retry"
			} else {
				"a plugin plan is already running"
			};
			return Err(StartError { message: message.into(), retry });
		}
		*admitting = Some(run_id.clone().unwrap_or_default());
		Admission(&state)
	};
	// This gate validates the installed manifests without running setup.
	tokio::select! {
		biased;
		() = state.closing.cancelled() => {
			return Err(StartError { message: "the agent is shutting down".into(), retry: true });
		},
		inspected = execute::inspect(&plan, &registry) => {
			inspected.map_err(StartError::refused)?;
		},
	}
	let mut status = state.status.lock().await;
	if state.closing.is_cancelled() {
		return Err(StartError { message: "the agent is shutting down".into(), retry: true });
	}
	if let Some(keep) = state.keep_runs {
		match prune_runs(Path::new(&root), keep) {
			Ok(removed) if !removed.is_empty() => {
				eprintln!("{PRODUCT_NAME}: removed {} old run directories", removed.len());
			},
			Ok(_) => {},
			Err(error) => {
				eprintln!("{PRODUCT_NAME}: could not remove old run directories: {error}")
			},
		}
	}
	let token = execute::Stop::default();
	*state.cancel.lock().await = Some(token.clone());
	static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
	let id = run_id.unwrap_or_else(|| {
		format!(
			"{}-{:020}",
			std::process::id(),
			NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
		)
	});
	*status = Status { id: id.clone(), state: "running".into(), ..Status::default() };
	*state
		.progress
		.lock()
		.map_err(|_| StartError::refused("progress lock poisoned"))? = None;
	let initial = status.clone();
	let run_state = state.clone();
	let task = tokio::spawn(async move {
		let state = run_state;
		let progress_state = state.clone();
		let progress = Arc::new(move |event: serde_json::Value| {
			if event["event"] == "phase"
				&& let Ok(mut phase) = progress_state.progress.lock()
			{
				*phase = event["phase"].as_str().map(str::to_owned);
			}
			sink(event);
		});
		// A panic in the run, or in its event sink, ends this inner task and surfaces as a
		// JoinError.
		let result = tokio::spawn(async move {
			execute::run_stoppable(plan, registry, Path::new(&root), token, progress, options).await
		})
		.await
		.unwrap_or_else(|_| Err(anyhow::anyhow!("run task panicked; see host/plugin diagnostics")));

		let mut status = state.status.lock().await;
		*status = match result {
			Ok(outcome) => Status {
				id: id.clone(),
				state: outcome.state.to_string(),
				outcome: Some(outcome),
				..Status::default()
			},
			Err(error) => Status {
				id: id.clone(),
				state: "failed".into(),
				error: Some(error.to_string()),
				..Status::default()
			},
		};
		*state.cancel.lock().await = None;
		let mut history = state.history.lock().await;
		history.insert(id, status.clone());
		while history.len() > 32 {
			history.pop_first();
		}
	});
	// Recorded before the status lock is released: a shutdown that sees this run as running
	// must also find its task to wait for.
	*state.task.lock().await = Some(task);
	drop(status);
	drop(admission);
	Ok(initial)
}

/// The status of a run this host started under `id`, if any.
async fn known_run(state: &State, id: &str) -> Option<Status> {
	let current = state.status.lock().await;
	if current.id == id {
		return Some(current.clone());
	}
	drop(current);
	state.history.lock().await.get(id).cloned()
}

/// Waits until the latest run's outcome is recorded. Returns at once if no run was started.
pub async fn finished(state: &State) {
	let task = state.task.lock().await.take();
	if let Some(task) = task {
		let _ = task.await;
	}
}

/// Deletes the oldest finished `run-<milliseconds>-<process>-<sequence>` directories under
/// `root` until at most `keep` finished ones remain, and returns the deleted paths. A run is
/// finished when its `execution.json` holds a final state and no process holds its
/// `run.lock`. Active runs, runs killed while stopping, symbolic links and other files are
/// never touched.
pub fn prune_runs(root: &Path, keep: usize) -> std::io::Result<Vec<PathBuf>> {
	let entries = match std::fs::read_dir(root) {
		Ok(entries) => entries,
		Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
		Err(error) => return Err(error),
	};
	let mut runs = Vec::new();
	for entry in entries {
		let entry = entry?;
		let Some(key) = entry.file_name().to_str().and_then(run_order) else { continue };
		if entry.file_type()?.is_dir() && run_is_over(&entry.path()) {
			runs.push((key, entry.path()));
		}
	}
	runs.sort();
	let excess = runs.len().saturating_sub(keep);
	let mut removed = Vec::new();
	for (_, path) in runs.into_iter().take(excess) {
		std::fs::remove_dir_all(&path)?;
		removed.push(path);
	}
	Ok(removed)
}

/// Whether a run directory holds a final outcome and no process is still running it.
fn run_is_over(directory: &Path) -> bool {
	let Ok(bytes) = std::fs::read(directory.join("execution.json")) else { return false };
	let Ok(outcome) = serde_json::from_slice::<execute::Outcome>(&bytes) else { return false };
	if outcome.state == execute::RunState::Stopping {
		return false;
	}
	match std::fs::File::open(directory.join(execute::RUN_LOCK)) {
		Ok(lock) => lock.try_lock().is_ok(),
		Err(error) => error.kind() == std::io::ErrorKind::NotFound,
	}
}

/// The start time, process and sequence of a run directory name, oldest first.
fn run_order(name: &str) -> Option<(u64, u64, u64)> {
	let mut parts = name.strip_prefix("run-")?.split('-');
	let key =
		(parts.next()?.parse().ok()?, parts.next()?.parse().ok()?, parts.next()?.parse().ok()?);
	parts.next().is_none().then_some(key)
}

/// A finished run from the history of the last 32.
async fn archived(state: &State, id: &str) -> Result<Status, String> {
	state
		.history
		.lock()
		.await
		.get(id)
		.cloned()
		.ok_or_else(|| "run ID is unknown or expired".into())
}

pub async fn status(state: &State, id: &str) -> Result<Status, String> {
	let current = state.status.lock().await;
	if current.id != id {
		return archived(state, id).await;
	}
	let mut result = current.clone();
	if result.state == "running" {
		result.phase = state.progress.lock().map_err(|_| "progress lock poisoned")?.clone();
	}
	Ok(result)
}

pub async fn stop(state: &State, id: &str) -> Result<Status, String> {
	stop_with(state, id, false).await
}

/// Stops a run. Without `force`, the run ends its phases and still runs teardown; with `force`,
/// teardown is interrupted too.
pub async fn stop_with(state: &State, id: &str, force: bool) -> Result<Status, String> {
	let current = state.status.lock().await;
	if current.id != id {
		return archived(state, id).await;
	}
	if let Some(stop) = state.cancel.lock().await.as_ref() {
		if force {
			stop.force(execute::STOPPED_EXIT_CODE);
		} else {
			stop.request(execute::STOPPED_EXIT_CODE);
		}
	}
	Ok(current.clone())
}

#[cfg(test)]
mod tests {
	use super::*;
	#[tokio::test]
	async fn panicked_run_releases_the_state_for_the_next_start() {
		let state = Arc::new(State::default());
		let root = std::env::temp_dir().join(format!("polkameter-panic-{}", std::process::id()));
		let xml = r#"<polkameter-plan xmlns="https://polkameter.dev/schema/plan" version="1" name="panic"><setup><step id="echo" use="core.echo"><input name="value" value="1"/></step></setup></polkameter-plan>"#;
		let first = start(
			xml.into(),
			root.to_string_lossy().into(),
			state.clone(),
			Arc::new(|_| panic!("injected sink panic")),
		)
		.await
		.unwrap();
		for _ in 0..200 {
			if status(&state, &first.id).await.unwrap().state != "running" {
				break;
			}
			tokio::time::sleep(std::time::Duration::from_millis(5)).await;
		}
		assert_eq!(status(&state, &first.id).await.unwrap().state, "failed");
		assert!(state.cancel.lock().await.is_none());
		let next =
			start(xml.into(), root.to_string_lossy().into(), state.clone(), Arc::new(|_| {}))
				.await
				.unwrap();
		for _ in 0..200 {
			if status(&state, &next.id).await.unwrap().state != "running" {
				break;
			}
			tokio::time::sleep(std::time::Duration::from_millis(5)).await;
		}
		assert_eq!(status(&state, &next.id).await.unwrap().state, "completed");
		std::fs::remove_dir_all(root).unwrap();
	}

	#[tokio::test]
	async fn finished_waits_for_the_recorded_outcome() {
		let state = Arc::new(State::default());
		finished(&state).await;
		let root = std::env::temp_dir().join(format!("polkameter-finished-{}", std::process::id()));
		let xml = r#"<polkameter-plan xmlns="https://polkameter.dev/schema/plan" version="1" name="finished"><setup><step id="echo" use="core.echo"><input name="value" value="1"/></step></setup></polkameter-plan>"#;
		let run = start(xml.into(), root.to_string_lossy().into(), state.clone(), Arc::new(|_| {}))
			.await
			.unwrap();
		finished(&state).await;
		assert_eq!(status(&state, &run.id).await.unwrap().state, "completed");
		std::fs::remove_dir_all(root).unwrap();
	}

	#[tokio::test]
	async fn keep_runs_prunes_old_run_directories_before_each_start() {
		let state = Arc::new(State { keep_runs: Some(2), ..State::default() });
		let root = std::env::temp_dir().join(format!("polkameter-keep-{}", std::process::id()));
		let xml = r#"<polkameter-plan xmlns="https://polkameter.dev/schema/plan" version="1" name="keep"><setup><step id="echo" use="core.echo"><input name="value" value="1"/></step></setup></polkameter-plan>"#;
		let mut directories = Vec::new();
		for _ in 0..4 {
			let run =
				start(xml.into(), root.to_string_lossy().into(), state.clone(), Arc::new(|_| {}))
					.await
					.unwrap();
			finished(&state).await;
			let outcome = status(&state, &run.id).await.unwrap().outcome.unwrap();
			directories.push(outcome.artifact_dir);
		}
		// Before the fourth start, three runs had finished: the oldest went. The run just before
		// a start always survives it, so its client can still fetch its report.
		let left = directories.iter().map(|directory| directory.is_dir()).collect::<Vec<_>>();
		assert_eq!(left, [false, true, true, true]);
		std::fs::remove_dir_all(root).unwrap();
	}

	fn run_directory(root: &Path, name: &str, state: Option<&str>) -> PathBuf {
		let directory = root.join(name);
		std::fs::create_dir(&directory).unwrap();
		if let Some(state) = state {
			let outcome = serde_json::json!({
				"artifact_dir": directory, "exit_code": 0, "state": state, "error": null,
			});
			std::fs::write(directory.join("execution.json"), outcome.to_string()).unwrap();
		}
		directory
	}

	#[test]
	fn pruning_removes_only_the_oldest_finished_runs() {
		let root = std::env::temp_dir().join(format!("polkameter-prune-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&root);
		std::fs::create_dir_all(&root).unwrap();
		// The numeric order differs from the text order: 999 < 1000.
		for (name, state) in [
			("run-1000-5-1", Some("completed")),
			("run-999-5-2", Some("failed")),
			("run-1000-5-3", Some("stopped")),
			("run-1001-4-1", Some("completed_with_failures")),
			// Not finished: no outcome yet, or killed while stopping.
			("run-1-1-1", None),
			("run-2-1-1", Some("stopping")),
			("run-x-1-1", Some("completed")),
			("notes", Some("completed")),
		] {
			run_directory(&root, name, state);
		}
		// Another process is still running this one: its lock is held.
		let active = run_directory(&root, "run-5-1-1", Some("completed"));
		let lock = std::fs::File::create(active.join(execute::RUN_LOCK)).unwrap();
		lock.lock().unwrap();
		std::fs::write(root.join("run-3-1-1"), "a file, not a run").unwrap();
		let removed = prune_runs(&root, 2).unwrap();
		assert_eq!(removed, vec![root.join("run-999-5-2"), root.join("run-1000-5-1")]);
		let mut left = std::fs::read_dir(&root)
			.unwrap()
			.map(|entry| entry.unwrap().file_name().into_string().unwrap())
			.collect::<Vec<_>>();
		left.sort();
		assert_eq!(
			left,
			[
				"notes",
				"run-1-1-1",
				"run-1000-5-3",
				"run-1001-4-1",
				"run-2-1-1",
				"run-3-1-1",
				"run-5-1-1",
				"run-x-1-1"
			]
		);
		assert!(prune_runs(&root, 2).unwrap().is_empty());
		assert_eq!(prune_runs(&root, 0).unwrap().len(), 2);
		// Once its process lets go, the finished run can be pruned.
		drop(lock);
		assert_eq!(prune_runs(&root, 0).unwrap(), vec![active]);
		assert!(prune_runs(&root.join("missing"), 0).unwrap().is_empty());
		std::fs::remove_dir_all(root).unwrap();
	}
}
