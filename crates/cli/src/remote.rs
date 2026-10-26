//! The remote agent: an authenticated HTTP API over the same engine, and the client the CLI and
//! the desktop app use to drive it. The client sends plan XML; the agent resolves plugins and
//! credentials on its own host.

use std::{net::SocketAddr, sync::Arc};

use axum::{
	Json, Router,
	extract::{Path, Request, State},
	http::{StatusCode, header::AUTHORIZATION},
	middleware::{self, Next},
	response::{IntoResponse, Response},
	routing::{get, post},
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
	let protected = Router::new()
		.route("/plugins", get(plugin_capabilities))
		.route("/preflight", post(plugin_preflight_handler))
		.route("/inspect", post(plugin_inspect_handler))
		.route("/runs", post(plugin_start_handler))
		.route("/runs/{id}", get(plugin_status_handler))
		.route("/runs/{id}/stop", post(plugin_stop_handler))
		.route_layer(middleware::from_fn_with_state(state.clone(), require_bearer));
	let app = Router::new()
		.route("/health", get(|| async { Json(serde_json::json!({"status":"ok"})) }))
		.merge(protected)
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

/// Lets a request through only when it carries the agent's bearer token.
async fn require_bearer(State(state): State<AgentState>, request: Request, next: Next) -> Response {
	let provided = request
		.headers()
		.get(AUTHORIZATION)
		.and_then(|value| value.to_str().ok())
		.and_then(|value| value.strip_prefix("Bearer "));
	if provided
		.is_some_and(|token| constant_time_eq(token.as_bytes(), state.bearer_token.as_bytes()))
	{
		next.run(request).await
	} else {
		(StatusCode::UNAUTHORIZED, "missing or invalid bearer token").into_response()
	}
}

/// Equal lengths, then a XOR fold over every byte, so the time does not reveal where they differ.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
	a.len() == b.len() && a.iter().zip(b).fold(0u8, |diff, (x, y)| diff | (x ^ y)) == 0
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
	#[serde(default)]
	pub options: polkameter_engine::execute::RunOptions,
}
async fn plugin_capabilities() -> AgentResult<Json<serde_json::Value>> {
	let registry = polkameter_engine::plugins::Registry::load().map_err(bad_request)?;
	Ok(Json(
		serde_json::json!({"protocol":1,"runOptions":{"failOnVerdict":true},"plugins":registry.plugins.iter().map(|(id,p)|serde_json::json!({"id":id,"version":p.version,"blake2":p.blake2})).collect::<Vec<_>>()}),
	))
}
async fn plugin_preflight_handler(
	Json(request): Json<PluginRequest>,
) -> AgentResult<Json<serde_json::Value>> {
	crate::plugin_application::preflight_xml(&request.xml)
		.await
		.map(Json)
		.map_err(bad_request)
}
async fn plugin_inspect_handler(
	Json(request): Json<PluginRequest>,
) -> AgentResult<Json<serde_json::Value>> {
	crate::plugin_application::inspect_xml(&request.xml)
		.await
		.map(Json)
		.map_err(bad_request)
}
async fn plugin_start_handler(
	State(state): State<AgentState>,
	Json(request): Json<PluginRequest>,
) -> AgentResult<Json<crate::plugin_application::Status>> {
	crate::plugin_application::start_with_options(
		request.xml,
		state.output_root,
		state.plugins.clone(),
		Arc::new(|_| {}),
		request.options,
	)
	.await
	.map(Json)
	.map_err(bad_request)
}
async fn plugin_status_handler(
	Path(id): Path<String>,
	State(state): State<AgentState>,
) -> AgentResult<Json<crate::plugin_application::Status>> {
	crate::plugin_application::status(&state.plugins, &id)
		.await
		.map(Json)
		.map_err(bad_request)
}
async fn plugin_stop_handler(
	Path(id): Path<String>,
	State(state): State<AgentState>,
) -> AgentResult<Json<crate::plugin_application::Status>> {
	crate::plugin_application::stop(&state.plugins, &id)
		.await
		.map(Json)
		.map_err(bad_request)
}

/// The client side: each call targets one agent endpoint.
pub async fn plugin_start(
	target: &RemoteRunnerTarget,
	xml: String,
) -> Result<crate::plugin_application::Status, String> {
	plugin_start_with_options(target, xml, Default::default()).await
}

/// Starts a remote run with an exit policy applied by the agent.
pub async fn plugin_start_with_options(
	target: &RemoteRunnerTarget,
	xml: String,
	options: polkameter_engine::execute::RunOptions,
) -> Result<crate::plugin_application::Status, String> {
	if options.fail_on_verdict {
		let capabilities: serde_json::Value =
			plugin_request(target, reqwest::Method::GET, "/plugins", None).await?;
		if capabilities["runOptions"]["failOnVerdict"] != true {
			return Err("remote agent does not support --fail-on-verdict; update the agent".into());
		}
	}
	plugin_request(target, reqwest::Method::POST, "/runs", Some(PluginRequest { xml, options }))
		.await
}
pub async fn plugin_status(
	id: &str,
	target: &RemoteRunnerTarget,
) -> Result<crate::plugin_application::Status, String> {
	plugin_request(target, reqwest::Method::GET, &run_path(id, "")?, None).await
}
pub async fn plugin_stop(
	id: &str,
	target: &RemoteRunnerTarget,
) -> Result<crate::plugin_application::Status, String> {
	plugin_request(target, reqwest::Method::POST, &run_path(id, "/stop")?, None).await
}
pub async fn plugin_preflight(
	target: &RemoteRunnerTarget,
	xml: String,
) -> Result<serde_json::Value, String> {
	plugin_request(
		target,
		reqwest::Method::POST,
		"/preflight",
		Some(PluginRequest { xml, options: Default::default() }),
	)
	.await
}
pub async fn plugin_inspect(
	target: &RemoteRunnerTarget,
	xml: String,
) -> Result<serde_json::Value, String> {
	plugin_request(
		target,
		reqwest::Method::POST,
		"/inspect",
		Some(PluginRequest { xml, options: Default::default() }),
	)
	.await
}

/// The endpoint `/runs/{id}{suffix}`, after rejecting run IDs that could escape the path.
fn run_path(id: &str, suffix: &str) -> Result<String, String> {
	if !is_safe_run_id(id) {
		return Err("invalid run ID".into());
	}
	Ok(format!("/runs/{id}{suffix}"))
}

async fn plugin_request<T: serde::de::DeserializeOwned>(
	target: &RemoteRunnerTarget,
	method: reqwest::Method,
	path: &str,
	body: Option<PluginRequest>,
) -> Result<T, String> {
	target.validate()?;
	static CLIENT: std::sync::OnceLock<reqwest::Client> = std::sync::OnceLock::new();
	let mut request = CLIENT
		.get_or_init(reqwest::Client::new)
		.request(method, format!("{}{path}", target.endpoint.trim_end_matches('/')))
		.bearer_auth(&target.bearer_token);
	if let Some(body) = body {
		request = request.json(&body);
	}
	decode_response(request.send().await.map_err(|error| error.to_string())?).await
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn remote_run_options_default_for_older_clients_and_round_trip() {
		let request: PluginRequest =
			serde_json::from_value(serde_json::json!({"xml":"plan"})).unwrap();
		assert!(!request.options.fail_on_verdict);
		let request = PluginRequest {
			xml: "plan".into(),
			options: polkameter_engine::execute::RunOptions { fail_on_verdict: true },
		};
		let json = serde_json::to_value(request).unwrap();
		assert_eq!(json["options"]["failOnVerdict"], true);
		assert!(serde_json::from_value::<PluginRequest>(json).unwrap().options.fail_on_verdict);
	}

	#[tokio::test]
	async fn fail_on_verdict_refuses_an_older_agent_before_starting() {
		let app = Router::new().route(
			"/plugins",
			get(|| async { Json(serde_json::json!({"protocol":1,"plugins":[]})) }),
		);
		let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
		let endpoint = format!("http://{}", listener.local_addr().unwrap());
		let server = tokio::spawn(async move {
			axum::serve(listener, app).await.unwrap();
		});
		let Err(error) = plugin_start_with_options(
			&RemoteRunnerTarget { endpoint, bearer_token: "test".into() },
			"plan".into(),
			polkameter_engine::execute::RunOptions { fail_on_verdict: true },
		)
		.await
		else {
			panic!("older agent accepted the policy")
		};
		assert_eq!(error, "remote agent does not support --fail-on-verdict; update the agent");
		server.abort();
	}

	#[test]
	fn remote_targets_require_tls_or_a_loopback_tunnel() {
		assert!(
			RemoteRunnerTarget {
				endpoint: "http://127.0.0.1:9901".into(),
				bearer_token: "token".into(),
			}
			.validate()
			.is_ok()
		);
		assert!(
			RemoteRunnerTarget {
				endpoint: "https://runner.example".into(),
				bearer_token: "token".into(),
			}
			.validate()
			.is_ok()
		);
		assert!(
			RemoteRunnerTarget {
				endpoint: "http://runner.example".into(),
				bearer_token: "token".into(),
			}
			.validate()
			.is_err()
		);
	}
}
