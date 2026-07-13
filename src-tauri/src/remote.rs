use std::{net::SocketAddr, sync::Arc};

use axum::{
	extract::{Path, State},
	http::{header::AUTHORIZATION, HeaderMap, StatusCode},
	routing::{get, post},
	Json, Router,
};
use serde::{Deserialize, Serialize};

use crate::{report, runner, scenario::ScenarioDocument};

pub const PROTOCOL_VERSION: u32 = 1;

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

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteRunRequest {
	pub protocol_version: u32,
	pub run_id: String,
	pub document: ScenarioDocument,
}

impl RemoteRunRequest {
	pub fn validate(&self) -> Result<(), String> {
		if self.protocol_version != PROTOCOL_VERSION {
			return Err(format!(
				"unsupported remote runner protocol {}; expected {PROTOCOL_VERSION}",
				self.protocol_version
			));
		}
		if !is_safe_run_id(&self.run_id) {
			return Err(
				"run ID may contain only ASCII letters, digits, hyphens, underscores, and periods"
					.into(),
			);
		}
		if self.document.signer_source.base_suri != "[redacted]" {
			return Err("remote run requests must contain a redacted signer source".into());
		}
		if let Some(issue) = self.document.validate().into_iter().next() {
			return Err(format!("scenario is invalid at {}: {}", issue.field, issue.message));
		}
		Ok(())
	}
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentHealth {
	pub protocol_version: u32,
	pub status: &'static str,
}

#[derive(Clone)]
struct AgentState {
	bearer_token: String,
	output_root: String,
	runner: Arc<runner::RunnerState>,
}

pub async fn serve_from_env() -> Result<(), String> {
	let bind = std::env::var("POLKAMETER_AGENT_BIND").unwrap_or_else(|_| "127.0.0.1:9901".into());
	let token = std::env::var("POLKAMETER_AGENT_TOKEN")
		.map_err(|_| "POLKAMETER_AGENT_TOKEN must be set before starting an agent".to_string())?;
	let output_root = std::env::var("POLKAMETER_AGENT_OUTPUT_ROOT")
		.unwrap_or_else(|_| "target/polkameter-agent-runs".into());
	serve(&bind, token, output_root).await
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
	let state =
		AgentState { bearer_token, output_root, runner: Arc::new(runner::RunnerState::default()) };
	let app = Router::new()
		.route("/v1/health", get(health))
		.route("/v1/runs", post(start_run))
		.route("/v1/runs/{run_id}", get(run_status))
		.route("/v1/runs/{run_id}/stop", post(stop_run))
		.route("/v1/runs/{run_id}/report", get(read_report))
		.with_state(state);
	let listener = tokio::net::TcpListener::bind(address)
		.await
		.map_err(|error| format!("could not bind remote agent: {error}"))?;
	axum::serve(listener, app)
		.await
		.map_err(|error| format!("remote agent stopped: {error}"))
}

pub async fn start(
	target: &RemoteRunnerTarget,
	document: ScenarioDocument,
	run_id: String,
) -> Result<runner::RunStatus, String> {
	target.validate()?;
	let request = RemoteRunRequest {
		protocol_version: PROTOCOL_VERSION,
		run_id,
		document: document.redacted_clone(),
	};
	let response = reqwest::Client::new()
		.post(format!("{}/v1/runs", target.endpoint.trim_end_matches('/')))
		.bearer_auth(&target.bearer_token)
		.json(&request)
		.send()
		.await
		.map_err(|error| format!("could not reach remote runner: {error}"))?;
	decode_response(response).await
}

pub async fn status(
	target: &RemoteRunnerTarget,
	run_id: &str,
) -> Result<runner::RunStatus, String> {
	target.validate()?;
	let response = reqwest::Client::new()
		.get(format!("{}/v1/runs/{run_id}", target.endpoint.trim_end_matches('/')))
		.bearer_auth(&target.bearer_token)
		.send()
		.await
		.map_err(|error| format!("could not reach remote runner: {error}"))?;
	decode_response(response).await
}

pub async fn stop(target: &RemoteRunnerTarget, run_id: &str) -> Result<runner::RunStatus, String> {
	target.validate()?;
	let response = reqwest::Client::new()
		.post(format!("{}/v1/runs/{run_id}/stop", target.endpoint.trim_end_matches('/')))
		.bearer_auth(&target.bearer_token)
		.send()
		.await
		.map_err(|error| format!("could not reach remote runner: {error}"))?;
	decode_response(response).await
}

pub async fn read_remote_report(
	target: &RemoteRunnerTarget,
	run_id: &str,
) -> Result<report::DashboardReport, String> {
	target.validate()?;
	let response = reqwest::Client::new()
		.get(format!("{}/v1/runs/{run_id}/report", target.endpoint.trim_end_matches('/')))
		.bearer_auth(&target.bearer_token)
		.send()
		.await
		.map_err(|error| format!("could not reach remote runner: {error}"))?;
	decode_response(response).await
}

async fn health() -> Json<AgentHealth> {
	Json(AgentHealth { protocol_version: PROTOCOL_VERSION, status: "ready" })
}

async fn start_run(
	State(state): State<AgentState>,
	headers: HeaderMap,
	Json(request): Json<RemoteRunRequest>,
) -> AgentResult<Json<runner::RunStatus>> {
	authorize(&headers, &state)?;
	request.validate().map_err(bad_request)?;
	let mut document = request.document;
	resolve_agent_signer(&mut document).map_err(bad_request)?;
	let status = runner::start(
		document,
		state.output_root,
		request.run_id,
		Arc::new(NoopEventSink),
		state.runner,
	)
	.await
	.map_err(bad_request)?;
	Ok(Json(status))
}

async fn run_status(
	State(state): State<AgentState>,
	headers: HeaderMap,
	Path(run_id): Path<String>,
) -> AgentResult<Json<runner::RunStatus>> {
	authorize(&headers, &state)?;
	let status = runner::status(state.runner).await;
	ensure_run_id(&status, &run_id)?;
	Ok(Json(status))
}

async fn stop_run(
	State(state): State<AgentState>,
	headers: HeaderMap,
	Path(run_id): Path<String>,
) -> AgentResult<Json<runner::RunStatus>> {
	authorize(&headers, &state)?;
	let status = runner::status(state.runner.clone()).await;
	ensure_run_id(&status, &run_id)?;
	runner::stop(state.runner).await.map(Json).map_err(bad_request)
}

async fn read_report(
	State(state): State<AgentState>,
	headers: HeaderMap,
	Path(run_id): Path<String>,
) -> AgentResult<Json<report::DashboardReport>> {
	authorize(&headers, &state)?;
	let status = runner::status(state.runner).await;
	ensure_run_id(&status, &run_id)?;
	let artifact_dir = status.artifact_dir.ok_or_else(not_found)?;
	report::read_dashboard(std::path::Path::new(&artifact_dir))
		.map(Json)
		.map_err(bad_request)
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

fn ensure_run_id(status: &runner::RunStatus, run_id: &str) -> AgentResult<()> {
	if status.run_id.as_deref() == Some(run_id) {
		Ok(())
	} else {
		Err(not_found())
	}
}

fn bad_request(error: impl ToString) -> (StatusCode, String) {
	(StatusCode::BAD_REQUEST, error.to_string())
}

fn not_found() -> (StatusCode, String) {
	(StatusCode::NOT_FOUND, "remote run was not found".into())
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

fn resolve_agent_signer(document: &mut ScenarioDocument) -> Result<(), String> {
	if let Ok(suri) = std::env::var("POLKAMETER_AGENT_SURI") {
		if suri.trim().is_empty() {
			return Err("POLKAMETER_AGENT_SURI must not be empty".into());
		}
		if document.signer_source.funding.is_some() && !suri.trim_start().starts_with("//") {
			return Err(
				"development funding requires POLKAMETER_AGENT_SURI to begin with //".into()
			);
		}
		document.signer_source.base_suri = suri;
		return Ok(());
	}
	crate::resolve_signer_profile(document)
}

struct NoopEventSink;

impl runner::RunEventSink for NoopEventSink {
	fn emit(&self, _: runner::RunEvent) {}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn remote_requests_require_redacted_signers_and_safe_run_ids() {
		let document = crate::artifacts::test_scenario();
		let raw = RemoteRunRequest {
			protocol_version: PROTOCOL_VERSION,
			run_id: "run-1".into(),
			document: document.clone(),
		};
		assert!(raw.validate().is_err());
		let redacted = RemoteRunRequest { document: document.redacted_clone(), ..raw };
		assert!(redacted.validate().is_ok());
		let invalid_id = RemoteRunRequest { run_id: "../../escape".into(), ..redacted };
		assert!(invalid_id.validate().is_err());
	}

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
