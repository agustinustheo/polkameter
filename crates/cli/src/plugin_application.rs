//! Frontend adapters for the shared XML plugin engine.
use polkameter_engine::{execute, plan::Plan, plugins::Registry};
use serde::Serialize;
use std::{path::Path, sync::Arc};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Default, serde::Deserialize, Serialize)]
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
	pub cancel: Mutex<Option<CancellationToken>>,
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
	let (plan, registry) = resolve(&xml).map_err(|e| e.to_string())?;
	let mut status = state.status.lock().await;
	if status.state == "running" {
		return Err("a plugin plan is already running".into());
	}
	// This gate validates the installed manifests without running setup.
	execute::inspect(&plan, &registry).await.map_err(|e| e.to_string())?;
	let token = CancellationToken::new();
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
	tokio::spawn(async move {
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
			execute::run(plan, registry, Path::new(&root), token, progress).await
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
	Ok(initial)
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
	let current = state.status.lock().await;
	if current.id != id {
		return archived(state, id).await;
	}
	if let Some(token) = state.cancel.lock().await.as_ref() {
		token.cancel();
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
}
