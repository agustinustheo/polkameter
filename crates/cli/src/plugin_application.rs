//! Frontend adapters for the shared XML plugin engine.
use crate::engine::{execute, plan::Plan, plugins::Registry};
use serde::Serialize;
use std::{
	path::{Path, PathBuf},
	sync::Arc,
};
use tokio::sync::Mutex;

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
	/// Keep at most this many `run-…` directories under the output root, deleting the oldest
	/// before a new run starts. `None` keeps them all.
	pub keep_runs: Option<usize>,
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
	let (plan, registry) = resolve(&xml).map_err(|e| e.to_string())?;
	let mut status = state.status.lock().await;
	if status.state == "running" {
		return Err("a plugin plan is already running".into());
	}
	// This gate validates the installed manifests without running setup.
	execute::inspect(&plan, &registry).await.map_err(|e| e.to_string())?;
	if let Some(keep) = state.keep_runs {
		// The new run adds one directory. No run is active while the status lock is held.
		match prune_runs(Path::new(&root), keep.saturating_sub(1)) {
			Ok(removed) if !removed.is_empty() => {
				eprintln!("Polkameter: removed {} old run directories", removed.len());
			},
			Ok(_) => {},
			Err(error) => eprintln!("Polkameter: could not remove old run directories: {error}"),
		}
	}
	let token = execute::Stop::default();
	*state.cancel.lock().await = Some(token.clone());
	static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
	let id = format!(
		"{}-{:020}",
		std::process::id(),
		NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
	);
	*status = Status { id: id.clone(), state: "running".into(), ..Status::default() };
	*state.progress.lock().map_err(|_| "progress lock poisoned")? = None;
	let initial = status.clone();
	drop(status);
	let task_state = state.clone();
	let task = tokio::spawn(async move {
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
	*task_state.task.lock().await = Some(task);
	Ok(initial)
}

/// Waits until the latest run's outcome is recorded. Returns at once if no run was started.
pub async fn finished(state: &State) {
	let task = state.task.lock().await.take();
	if let Some(task) = task {
		let _ = task.await;
	}
}

/// Deletes the oldest `run-<milliseconds>-<process>-<sequence>` directories under `root` until
/// at most `keep` remain, and returns the deleted paths. Other entries, including symbolic
/// links and files, are never touched.
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
		if entry.file_type()?.is_dir() {
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
		let left = directories.iter().map(|directory| directory.is_dir()).collect::<Vec<_>>();
		assert_eq!(left, [false, false, true, true]);
		std::fs::remove_dir_all(root).unwrap();
	}

	#[test]
	fn pruning_keeps_the_newest_runs_and_ignores_everything_else() {
		let root = std::env::temp_dir().join(format!("polkameter-prune-{}", std::process::id()));
		let _ = std::fs::remove_dir_all(&root);
		std::fs::create_dir_all(&root).unwrap();
		// The numeric order differs from the text order: 999 < 1000.
		for name in
			["run-1000-5-1", "run-999-5-2", "run-1000-5-3", "run-1001-4-1", "run-x-1-1", "notes"]
		{
			std::fs::create_dir(root.join(name)).unwrap();
		}
		std::fs::write(root.join("run-1-1-1"), "a file, not a run").unwrap();
		let removed = prune_runs(&root, 2).unwrap();
		assert_eq!(removed, vec![root.join("run-999-5-2"), root.join("run-1000-5-1")]);
		let mut left = std::fs::read_dir(&root)
			.unwrap()
			.map(|entry| entry.unwrap().file_name().into_string().unwrap())
			.collect::<Vec<_>>();
		left.sort();
		assert_eq!(left, ["notes", "run-1-1-1", "run-1000-5-3", "run-1001-4-1", "run-x-1-1"]);
		assert!(prune_runs(&root, 2).unwrap().is_empty());
		assert_eq!(prune_runs(&root, 0).unwrap().len(), 2);
		assert!(prune_runs(&root.join("missing"), 0).unwrap().is_empty());
		std::fs::remove_dir_all(root).unwrap();
	}
}
