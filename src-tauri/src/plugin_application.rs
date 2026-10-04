//! Frontend adapters for the shared XML plugin engine.
use futures::FutureExt;
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
pub async fn start(
	xml: String,
	root: String,
	state: Arc<State>,
	sink: execute::EventSink,
) -> Result<Status, String> {
	let plan = Plan::parse(&xml).map_err(|e| e.to_string())?;
	let registry = Registry::load().map_err(|e| e.to_string())?;
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
	*status = Status { id: id.clone(), phase: None, state: "running".into(), ..Status::default() };
	*state.progress.lock().map_err(|_| "progress lock poisoned")? = None;
	let initial = status.clone();
	drop(status);
	tokio::spawn(async move {
		let progress_state = state.clone();
		let progress = Arc::new(move |event: serde_json::Value| {
			if event["event"] == "phase" {
				if let Ok(mut phase) = progress_state.progress.lock() {
					*phase = event["phase"].as_str().map(str::to_owned);
				}
			}
			sink(event);
		});
		let result = std::panic::AssertUnwindSafe(async {
			execute::run(plan, registry, Path::new(&root), token, progress)
				.await
				.and_then(finalize_report)
		})
		.catch_unwind()
		.await
		.unwrap_or_else(|_| Err(anyhow::anyhow!("run task panicked; see host/plugin diagnostics")));

		let mut status = state.status.lock().await;
		*status = match result {
			Ok(outcome) => Status {
				id: id.clone(),
				phase: None,
				state: outcome.state.clone(),
				outcome: Some(outcome),
				error: None,
			},
			Err(error) => Status {
				id: id.clone(),
				phase: None,
				state: "failed".into(),
				error: Some(error.to_string()),
				outcome: None,
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

/// One report-failure policy for headless, desktop and agent runs.
pub fn finalize_report(mut outcome: execute::Outcome) -> anyhow::Result<execute::Outcome> {
	if let Err(error) = crate::report::write(&outcome.artifact_dir) {
		outcome.state = "failed".into();
		outcome.exit_code = 1;
		outcome.error = Some(format!("report generation failed: {error}"));
		std::fs::write(
			outcome.artifact_dir.join("execution.json"),
			serde_json::to_vec_pretty(&outcome)?,
		)?;
	}
	Ok(outcome)
}

pub async fn status(state: &State, id: &str) -> Result<Status, String> {
	let status = state.status.lock().await;
	if status.id == id {
		let mut result = status.clone();
		if result.state == "running" {
			result.phase = state.progress.lock().map_err(|_| "progress lock poisoned")?.clone();
		}
		return Ok(result);
	}
	state
		.history
		.lock()
		.await
		.get(id)
		.cloned()
		.ok_or_else(|| "run ID is unknown or expired".into())
}
pub async fn stop(state: &State, id: &str) -> Result<Status, String> {
	let status = state.status.lock().await;
	if status.id != id {
		return state
			.history
			.lock()
			.await
			.get(id)
			.cloned()
			.ok_or_else(|| "run ID is unknown or expired".into());
	}
	if let Some(token) = state.cancel.lock().await.as_ref() {
		token.cancel();
	}
	Ok(status.clone())
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
