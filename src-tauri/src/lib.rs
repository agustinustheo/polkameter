//! The desktop app and the headless CLI over the shared XML plan engine.

pub mod cli;
mod plugin_application;
mod remote;
mod report;

use tauri::{Emitter, Manager};

pub fn run() {
	tauri::Builder::<tauri::Wry>::default()
		.manage(std::sync::Arc::new(plugin_application::State::default()))
		.setup(|app: &mut tauri::App<tauri::Wry>| {
			let Some(window) = app.get_webview_window("main") else {
				return Ok(());
			};
			// Keep first launch on macOS's primary coordinate space. Display-relative
			// centering can resurrect a stale virtual-desktop location after monitor changes.
			window.set_position(tauri::Position::Physical(tauri::PhysicalPosition::new(40, 40)))?;
			Ok(())
		})
		.invoke_handler(tauri::generate_handler![
			open_plugin_plan,
			save_plugin_plan,
			inspect_plugin_plan,
			preflight_plugin_plan,
			start_plugin_plan,
			get_plugin_status,
			stop_plugin_plan
		])
		.run(tauri::generate_context!())
		.expect("error while running Polkameter");
}

#[tauri::command]
fn open_plugin_plan() -> Result<Option<String>, String> {
	let Some(path) = rfd::FileDialog::new()
		.set_title("Open XML plugin plan")
		.add_filter("XML", &["xml"])
		.pick_file()
	else {
		return Ok(None);
	};
	let xml = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
	polkameter_engine::plan::Plan::parse(&xml).map_err(|e| e.to_string())?;
	Ok(Some(xml))
}
#[tauri::command]
fn save_plugin_plan(xml: String) -> Result<Option<String>, String> {
	polkameter_engine::plan::Plan::parse(&xml).map_err(|e| e.to_string())?;
	let Some(path) = rfd::FileDialog::new()
		.set_title("Save XML plugin plan")
		.set_file_name("scenario.polkameter.xml")
		.add_filter("XML", &["xml"])
		.save_file()
	else {
		return Ok(None);
	};
	std::fs::write(&path, xml).map_err(|e| e.to_string())?;
	Ok(Some(path.display().to_string()))
}
#[tauri::command]
async fn inspect_plugin_plan(
	xml: String,
	target: Option<remote::RemoteRunnerTarget>,
) -> Result<serde_json::Value, String> {
	if let Some(target) = target {
		return remote::plugin_inspect(&target, xml).await;
	}
	let plan = polkameter_engine::plan::Plan::parse(&xml).map_err(|e| e.to_string())?;
	let registry = polkameter_engine::plugins::Registry::load().map_err(|e| e.to_string())?;
	polkameter_engine::execute::inspect(&plan, &registry)
		.await
		.map_err(|e| e.to_string())
}
#[tauri::command]
async fn preflight_plugin_plan(
	xml: String,
	target: Option<remote::RemoteRunnerTarget>,
) -> Result<serde_json::Value, String> {
	if let Some(target) = target {
		return remote::plugin_preflight(&target, xml).await;
	}
	let plan = polkameter_engine::plan::Plan::parse(&xml).map_err(|e| e.to_string())?;
	let registry = polkameter_engine::plugins::Registry::load().map_err(|e| e.to_string())?;
	polkameter_engine::execute::preflight(&plan, &registry)
		.await
		.map_err(|e| e.to_string())
}
#[tauri::command]
async fn start_plugin_plan(
	xml: String,
	target: Option<remote::RemoteRunnerTarget>,
	app: tauri::AppHandle,
	state: tauri::State<'_, std::sync::Arc<plugin_application::State>>,
) -> Result<plugin_application::Status, String> {
	if let Some(target) = target {
		return remote::plugin_start(&target, xml).await;
	}
	plugin_application::start(
		xml,
		app.path()
			.app_local_data_dir()
			.map_err(|error| error.to_string())?
			.join("runs")
			.to_string_lossy()
			.into_owned(),
		state.inner().clone(),
		std::sync::Arc::new(move |event| {
			let _ = app.emit("plugin-run-event", event);
		}),
	)
	.await
}
#[tauri::command]
async fn get_plugin_status(
	run_id: String,
	target: Option<remote::RemoteRunnerTarget>,
	state: tauri::State<'_, std::sync::Arc<plugin_application::State>>,
) -> Result<plugin_application::Status, String> {
	if let Some(target) = target {
		return remote::plugin_status(&run_id, &target).await;
	}
	plugin_application::status(state.inner(), &run_id).await
}
#[tauri::command]
async fn stop_plugin_plan(
	run_id: String,
	target: Option<remote::RemoteRunnerTarget>,
	state: tauri::State<'_, std::sync::Arc<plugin_application::State>>,
) -> Result<plugin_application::Status, String> {
	if let Some(target) = target {
		return remote::plugin_stop(&run_id, &target).await;
	}
	plugin_application::stop(state.inner(), &run_id).await
}
