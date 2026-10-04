//! The remote agent: an authenticated HTTP API over the same engine, and the client the CLI and
//! the desktop app use to drive it. The client sends plan XML; the agent resolves plugins and
//! credentials on its own host.

use std::{net::SocketAddr, sync::Arc};

use axum::{
	extract::State,
	http::{header::AUTHORIZATION, HeaderMap, StatusCode},
	routing::{get, post},
	Json, Router,
};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteRunnerTarget {
	pub endpoint: String,
	pub bearer_token: String,
}

impl RemoteRunnerTarget {
	pub fn validate(&self) -> Result<(), String> {
		let endpoint = self.endpoint.trim_end_matches('/');
		if self.bearer_token.trim().is_empty() {
			return Err("remote runner bearer token must not be empty".into());
		}
		if endpoint.starts_with("https://") || is_loopback_http(endpoint) {
			return Ok(());
		}
		Err("remote runner endpoint must use https://, or http:// only through a loopback SSH tunnel"
			.into())
	}
}

#[derive(Clone)]
struct AgentState {
	bearer_token: String,
	output_root: String,
	plugins: Arc<crate::plugin_application::State>,
}

pub async fn serve(bind: &str, bearer_token: String, output_root: String) -> Result<(), String> {
	if bearer_token.trim().is_empty() {
		return Err("agent bearer token must not be empty".into());
	}
	let address = bind
		.parse::<SocketAddr>()
		.map_err(|error| format!("invalid agent bind address: {error}"))?;
	if !address.ip().is_loopback() {
		return Err(
			"agent binds only to a loopback address; use an SSH tunnel or TLS terminator".into()
		);
	}
	let state = AgentState {
		bearer_token,
		output_root,
		plugins: Arc::new(crate::plugin_application::State::default()),
	};
	let app = Router::new()
		.route("/health", get(|| async { Json(serde_json::json!({"status":"ok"})) }))
		.route("/plugins", get(plugin_capabilities))
		.route("/preflight", post(plugin_preflight_handler))
		.route("/inspect", post(plugin_inspect_handler))
		.route("/runs", post(plugin_start_handler))
		.route("/runs/{id}", get(plugin_status_handler))
		.route("/runs/{id}/stop", post(plugin_stop_handler))
		.with_state(state);
	let listener = tokio::net::TcpListener::bind(address)
		.await
		.map_err(|error| format!("could not bind remote agent: {error}"))?;
	axum::serve(listener, app)
		.await
		.map_err(|error| format!("remote agent stopped: {error}"))
}

async fn decode_response<T: for<'de> Deserialize<'de>>(
	response: reqwest::Response,
) -> Result<T, String> {
	let status = response.status();
	let body = response
		.text()
		.await
		.map_err(|error| format!("could not read remote runner response: {error}"))?;
	if !status.is_success() {
		return Err(format!("remote runner returned {status}: {body}"));
	}
	serde_json::from_str(&body)
		.map_err(|error| format!("could not decode remote runner response: {error}"))
}

type AgentResult<T> = Result<T, (StatusCode, String)>;

fn authorize(headers: &HeaderMap, state: &AgentState) -> AgentResult<()> {
	let provided = headers
		.get(AUTHORIZATION)
		.and_then(|value| value.to_str().ok())
		.and_then(|value| value.strip_prefix("Bearer "));
	if provided == Some(state.bearer_token.as_str()) {
		Ok(())
	} else {
		Err((StatusCode::UNAUTHORIZED, "missing or invalid bearer token".into()))
	}
}

fn bad_request(error: impl ToString) -> (StatusCode, String) {
	(StatusCode::BAD_REQUEST, error.to_string())
}

fn is_safe_run_id(value: &str) -> bool {
	!value.is_empty()
		&& value.len() <= 128
		&& value.chars().all(|character| {
			character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
		})
}

fn is_loopback_http(endpoint: &str) -> bool {
	endpoint.starts_with("http://127.0.0.1:")
		|| endpoint.starts_with("http://localhost:")
		|| endpoint.starts_with("http://[::1]:")
}

#[derive(Deserialize, Serialize)]
pub struct PluginRequest {
	pub xml: String,
}
async fn plugin_capabilities(
	State(state): State<AgentState>,
	headers: HeaderMap,
) -> AgentResult<Json<serde_json::Value>> {
	authorize(&headers, &state)?;
	let registry = polkameter_engine::plugins::Registry::load().map_err(bad_request)?;
	Ok(Json(
		serde_json::json!({"protocol":1,"plugins":registry.plugins.iter().map(|(id,p)|serde_json::json!({"id":id,"version":p.version,"blake2":p.blake2})).collect::<Vec<_>>()}),
	))
}
async fn plugin_preflight_handler(
	State(state): State<AgentState>,
	headers: HeaderMap,
	Json(request): Json<PluginRequest>,
) -> AgentResult<Json<serde_json::Value>> {
	authorize(&headers, &state)?;
	let plan = polkameter_engine::plan::Plan::parse(&request.xml).map_err(bad_request)?;
	let registry = polkameter_engine::plugins::Registry::load().map_err(bad_request)?;
	polkameter_engine::execute::preflight(&plan, &registry)
		.await
		.map(Json)
		.map_err(bad_request)
}
async fn plugin_start_handler(
	State(state): State<AgentState>,
	headers: HeaderMap,
	Json(request): Json<PluginRequest>,
) -> AgentResult<Json<crate::plugin_application::Status>> {
	authorize(&headers, &state)?;
	crate::plugin_application::start(
		request.xml,
		state.output_root,
		state.plugins.clone(),
		Arc::new(|_| {}),
	)
	.await
	.map(Json)
	.map_err(bad_request)
}
async fn plugin_status_handler(
	axum::extract::Path(id): axum::extract::Path<String>,
	State(state): State<AgentState>,
	headers: HeaderMap,
) -> AgentResult<Json<crate::plugin_application::Status>> {
	authorize(&headers, &state)?;
	crate::plugin_application::status(&state.plugins, &id)
		.await
		.map(Json)
		.map_err(bad_request)
}
async fn plugin_stop_handler(
	axum::extract::Path(id): axum::extract::Path<String>,
	State(state): State<AgentState>,
	headers: HeaderMap,
) -> AgentResult<Json<crate::plugin_application::Status>> {
	authorize(&headers, &state)?;
	crate::plugin_application::stop(&state.plugins, &id)
		.await
		.map(Json)
		.map_err(bad_request)
}
pub async fn plugin_start(
	target: &RemoteRunnerTarget,
	xml: String,
) -> Result<crate::plugin_application::Status, String> {
	plugin_request(target, reqwest::Method::POST, "/runs", Some(xml)).await
}
pub async fn plugin_status(
	id: &str,
	target: &RemoteRunnerTarget,
) -> Result<crate::plugin_application::Status, String> {
	if !is_safe_run_id(id) {
		return Err("invalid run ID".into());
	}
	plugin_request(target, reqwest::Method::GET, &format!("/runs/{id}"), None).await
}
pub async fn plugin_stop(
	id: &str,
	target: &RemoteRunnerTarget,
) -> Result<crate::plugin_application::Status, String> {
	if !is_safe_run_id(id) {
		return Err("invalid run ID".into());
	}
	plugin_request(target, reqwest::Method::POST, &format!("/runs/{id}/stop"), None).await
}
pub async fn plugin_preflight(
	target: &RemoteRunnerTarget,
	xml: String,
) -> Result<serde_json::Value, String> {
	plugin_request(target, reqwest::Method::POST, "/preflight", Some(xml)).await
}

async fn plugin_inspect_handler(
	State(state): State<AgentState>,
	headers: HeaderMap,
	Json(request): Json<PluginRequest>,
) -> AgentResult<Json<serde_json::Value>> {
	authorize(&headers, &state)?;
	let plan = polkameter_engine::plan::Plan::parse(&request.xml).map_err(bad_request)?;
	let registry = polkameter_engine::plugins::Registry::load().map_err(bad_request)?;
	polkameter_engine::execute::inspect(&plan, &registry)
		.await
		.map(Json)
		.map_err(bad_request)
}
pub async fn plugin_inspect(
	target: &RemoteRunnerTarget,
	xml: String,
) -> Result<serde_json::Value, String> {
	plugin_request(target, reqwest::Method::POST, "/inspect", Some(xml)).await
}

async fn plugin_request<T: serde::de::DeserializeOwned>(
	target: &RemoteRunnerTarget,
	method: reqwest::Method,
	path: &str,
	xml: Option<String>,
) -> Result<T, String> {
	target.validate()?;
	static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
	let mut request = CLIENT
		.get_or_init(reqwest::Client::new)
		.request(method, format!("{}{path}", target.endpoint.trim_end_matches('/')))
		.bearer_auth(&target.bearer_token);
	if let Some(xml) = xml {
		request = request.json(&PluginRequest { xml });
	}
	decode_response(request.send().await.map_err(|error| error.to_string())?).await
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn remote_targets_require_tls_or_a_loopback_tunnel() {
		assert!(RemoteRunnerTarget {
			endpoint: "http://127.0.0.1:9901".into(),
			bearer_token: "token".into(),
		}
		.validate()
		.is_ok());
		assert!(RemoteRunnerTarget {
			endpoint: "https://runner.example".into(),
			bearer_token: "token".into(),
		}
		.validate()
		.is_ok());
		assert!(RemoteRunnerTarget {
			endpoint: "http://runner.example".into(),
			bearer_token: "token".into(),
		}
		.validate()
		.is_err());
	}
}
