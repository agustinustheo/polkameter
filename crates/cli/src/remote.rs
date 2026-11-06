//! The remote agent: an authenticated HTTP API over the same engine, and the client the CLI and
//! the desktop app use to drive it. The client sends plan XML; the agent resolves plugins and
//! credentials on its own host.

use std::{
	net::{IpAddr, SocketAddr},
	sync::Arc,
	time::Duration,
};

use axum::{
	Json, Router,
	extract::{Path, Query, Request, State},
	http::{StatusCode, header::AUTHORIZATION},
	middleware::{self, Next},
	response::{IntoResponse, Response},
	routing::{get, post},
};
use serde::{Deserialize, Serialize};
use tokio::time::Instant;

use crate::{
	PRODUCT_NAME,
	shutdown::{FORCE_LIMIT, Signal, Signals},
};

/// Checks that an agent token is long and varied enough to resist guessing.
pub fn check_token(token: &str) -> Result<(), String> {
	let token = token.trim();
	let distinct = token.chars().collect::<std::collections::BTreeSet<_>>().len();
	if token.chars().count() < MIN_TOKEN_CHARS || distinct < MIN_TOKEN_DISTINCT {
		return Err(format!(
			"must have at least {MIN_TOKEN_CHARS} characters, at least {MIN_TOKEN_DISTINCT} of them different; generate one with `openssl rand -hex 32`"
		));
	}
	Ok(())
}

/// Shortest bearer token the agent accepts, in characters.
pub const MIN_TOKEN_CHARS: usize = 32;
/// How long the client waits to connect to the agent.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long one request to the agent may take, including its response body.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(60);
/// How long an inspect or preflight request may take.
pub const CHECK_TIMEOUT: Duration = Duration::from_secs(600);
/// How long one stop request may take.
pub const STOP_TIMEOUT: Duration = Duration::from_secs(5);
/// Fewest distinct characters an agent token must have.
pub const MIN_TOKEN_DISTINCT: usize = 8;
/// How long a status poll may keep failing before the client gives up on the run.
pub const RETRY_BUDGET: Duration = Duration::from_secs(60);
/// Longest wait between two retries of a failed status poll.
const MAX_RETRY_DELAY: Duration = Duration::from_secs(5);
/// Time between status polls of a running remote run.
const POLL_INTERVAL: Duration = Duration::from_millis(300);
/// How long a stopping agent keeps serving after its run ended, until a client reads the outcome.
const OUTCOME_DRAIN: Duration = Duration::from_secs(5);

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteRunnerTarget {
	pub endpoint: String,
	pub bearer_token: String,
}

impl RemoteRunnerTarget {
	pub fn validate(&self) -> Result<(), String> {
		if self.bearer_token.trim().is_empty() {
			return Err("remote runner bearer token must not be empty".into());
		}
		self.base_url().map(|_| ())
	}

	/// The endpoint, parsed and checked, without a trailing `/`. HTTPS may go to any host. Plain
	/// HTTP must go to a loopback address (`127.0.0.0/8`, `::1` or `localhost`), as through an
	/// SSH tunnel. User information, queries and fragments are refused.
	fn base_url(&self) -> Result<String, String> {
		let url = reqwest::Url::parse(self.endpoint.trim())
			.map_err(|error| format!("invalid remote runner endpoint: {error}"))?;
		if !url.username().is_empty() || url.password().is_some() {
			return Err("remote runner endpoint must not contain user information (user@ or user:password@)".into());
		}
		if url.query().is_some() || url.fragment().is_some() {
			return Err("remote runner endpoint must not contain a query or fragment".into());
		}
		let host = url.host_str().unwrap_or_default();
		let allowed = match url.scheme() {
			"https" => !host.is_empty(),
			"http" => is_loopback_host(host),
			_ => false,
		};
		if !allowed {
			return Err("remote runner endpoint must use https://, or http:// only through a loopback SSH tunnel".into());
		}
		Ok(url.as_str().trim_end_matches('/').to_owned())
	}
}

/// Whether a parsed URL host names this machine. IPv6 hosts arrive in brackets.
fn is_loopback_host(host: &str) -> bool {
	let address = host.strip_prefix('[').and_then(|h| h.strip_suffix(']')).unwrap_or(host);
	match address.parse::<IpAddr>() {
		Ok(ip) => ip.is_loopback(),
		Err(_) => host.eq_ignore_ascii_case("localhost"),
	}
}

#[derive(Clone)]
struct AgentState {
	bearer_token: String,
	output_root: String,
	plugins: Arc<crate::plugin_application::State>,
	/// Notified when a stopping agent serves a finished run's status.
	delivered: Arc<tokio::sync::Notify>,
}

/// Settings of `agent serve` beyond its address and token.
#[derive(Clone, Debug)]
pub struct AgentSettings {
	/// Where run directories go.
	pub output_root: String,
	/// Keep at most this many run directories; `None` keeps them all.
	pub keep_runs: Option<usize>,
	/// How long an active run may take to stop after the first signal.
	pub grace: Duration,
}

/// Serves the agent until a stop signal. The first signal stops the active run, which still
/// runs its teardown within `grace`; a second signal or the end of the grace period interrupts
/// teardown. New runs are refused meanwhile. Returns the first signal.
pub async fn serve(
	bind: &str,
	bearer_token: String,
	settings: AgentSettings,
	signals: &mut Signals,
) -> Result<Signal, String> {
	check_token(&bearer_token).map_err(|error| format!("agent bearer token {error}"))?;
	let address = bind
		.parse::<SocketAddr>()
		.map_err(|error| format!("invalid agent bind address: {error}"))?;
	if !address.ip().is_loopback() {
		return Err(
			"agent binds only to a loopback address; use an SSH tunnel or TLS terminator".into()
		);
	}
	if settings.keep_runs == Some(0) {
		return Err("--keep-runs must be at least 1".into());
	}
	let plugins = Arc::new(crate::plugin_application::State {
		keep_runs: settings.keep_runs,
		..Default::default()
	});
	let state = AgentState {
		bearer_token,
		output_root: settings.output_root,
		plugins: plugins.clone(),
		delivered: Arc::default(),
	};
	let delivered = state.delivered.clone();
	let app = router(state);
	let listener = tokio::net::TcpListener::bind(address)
		.await
		.map_err(|error| format!("could not bind remote agent: {error}"))?;
	let (close, closed) = tokio::sync::oneshot::channel::<()>();
	let mut server = tokio::spawn(async move {
		axum::serve(listener, app)
			.with_graceful_shutdown(async {
				let _ = closed.await;
			})
			.await
	});
	let first = tokio::select! {
		result = &mut server => {
			return Err(match result {
				Ok(Ok(())) => "remote agent stopped".into(),
				Ok(Err(error)) => format!("remote agent stopped: {error}"),
				Err(error) => format!("remote agent stopped: {error}"),
			});
		},
		signal = signals.recv() => signal,
	};
	// Refuse new runs and end any admission in progress before looking for the active run.
	plugins.closing.cancel();
	let active = plugins.status.lock().await.state == "running";
	if active {
		eprintln!(
			"{PRODUCT_NAME}: received {}; stopping the active run (teardown has up to {} s; send the signal again to stop now)",
			first.name(),
			settings.grace.as_secs()
		);
	} else {
		eprintln!("{PRODUCT_NAME}: received {}; shutting down", first.name());
	}
	// Keep serving while the run stops, so its client can still read the outcome.
	let stop = plugins.cancel.lock().await.clone();
	let supervised = crate::shutdown::supervise(
		crate::plugin_application::finished(&plugins),
		signals,
		settings.grace,
		Some(first),
		|signal| {
			if let Some(stop) = &stop {
				stop.request(signal.exit_code());
			}
		},
		|signal| {
			eprintln!("{PRODUCT_NAME}: stopping now; teardown is interrupted");
			if let Some(stop) = &stop {
				stop.force(signal.exit_code());
			}
		},
	)
	.await;
	if supervised.result.is_none() {
		eprintln!(
			"{PRODUCT_NAME}: the run did not record its outcome within {} s of a forced stop",
			FORCE_LIMIT.as_secs()
		);
	} else if active {
		// Give the run's client a moment to read the outcome before the agent goes away.
		let _ = tokio::time::timeout(OUTCOME_DRAIN, delivered.notified()).await;
	}
	let _ = close.send(());
	let _ = tokio::time::timeout(Duration::from_secs(5), server).await;
	Ok(first)
}

fn router(state: AgentState) -> Router {
	let protected = Router::new()
		.route("/plugins", get(plugin_capabilities))
		.route("/preflight", post(plugin_preflight_handler))
		.route("/inspect", post(plugin_inspect_handler))
		.route("/runs", post(plugin_start_handler))
		.route("/runs/{id}", get(plugin_status_handler))
		.route("/runs/{id}/junit", get(plugin_junit_handler))
		.route("/runs/{id}/stop", post(plugin_stop_handler))
		.route_layer(middleware::from_fn_with_state(state.clone(), require_bearer));
	Router::new()
		.route("/health", get(|| async { Json(serde_json::json!({"status":"ok"})) }))
		.merge(protected)
		.with_state(state)
}

async fn decode_response<T: for<'de> Deserialize<'de>>(
	response: reqwest::Response,
) -> Result<T, RequestError> {
	let status = response.status();
	let body = response.text().await.map_err(|error| RequestError {
		message: format!("could not read remote runner response: {error}"),
		retryable: true,
	})?;
	if !status.is_success() {
		return Err(RequestError {
			message: format!("remote runner returned {status}: {body}"),
			retryable: status.is_server_error()
				|| status == reqwest::StatusCode::REQUEST_TIMEOUT
				|| status == reqwest::StatusCode::TOO_MANY_REQUESTS,
		});
	}
	serde_json::from_str(&body).map_err(|error| RequestError {
		message: format!("could not decode remote runner response: {error}"),
		retryable: false,
	})
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

/// A run ID that is safe in a URL path: no separators, and not a `.` or `..` segment.
fn is_safe_run_id(value: &str) -> bool {
	!value.is_empty()
		&& value.len() <= 128
		&& !matches!(value, "." | "..")
		&& value.chars().all(|character| {
			character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
		})
}

#[derive(Deserialize, Serialize)]
pub struct PluginRequest {
	pub xml: String,
	#[serde(default)]
	pub options: crate::engine::execute::RunOptions,
	/// A client-chosen run ID. Starting twice with the same ID starts one run.
	#[serde(default, rename = "runId", skip_serializing_if = "Option::is_none")]
	pub run_id: Option<String>,
}
async fn plugin_capabilities() -> AgentResult<Json<serde_json::Value>> {
	let registry = crate::engine::plugins::Registry::load().map_err(bad_request)?;
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
	if state.plugins.closing.is_cancelled() {
		return Err((StatusCode::SERVICE_UNAVAILABLE, "the agent is shutting down".into()));
	}
	if request.run_id.as_deref().is_some_and(|id| !is_safe_run_id(id)) {
		return Err(bad_request("invalid run ID"));
	}
	crate::plugin_application::start_with_id(
		request.xml,
		state.output_root,
		state.plugins.clone(),
		Arc::new(|_| {}),
		request.options,
		request.run_id,
	)
	.await
	.map(Json)
	.map_err(|error| {
		let status =
			if error.retry { StatusCode::SERVICE_UNAVAILABLE } else { StatusCode::BAD_REQUEST };
		(status, error.message)
	})
}
async fn plugin_status_handler(
	Path(id): Path<String>,
	State(state): State<AgentState>,
) -> AgentResult<Json<crate::plugin_application::Status>> {
	let status = crate::plugin_application::status(&state.plugins, &id)
		.await
		.map_err(bad_request)?;
	if state.plugins.closing.is_cancelled() && status.state != "running" {
		state.delivered.notify_one();
	}
	Ok(Json(status))
}
// Serve only the fixed report file of a completed run known to this agent.
async fn plugin_junit_handler(
	Path(id): Path<String>,
	State(state): State<AgentState>,
) -> AgentResult<Json<String>> {
	let status = crate::plugin_application::status(&state.plugins, &id)
		.await
		.map_err(bad_request)?;
	let outcome = status.outcome.ok_or_else(|| bad_request("run has no completed report"))?;
	std::fs::read_to_string(outcome.artifact_dir.join("junit.xml"))
		.map(Json)
		.map_err(bad_request)
}

/// `POST /runs/{id}/stop?force=true` also interrupts teardown.
#[derive(Default, Deserialize)]
struct StopQuery {
	#[serde(default)]
	force: bool,
}

async fn plugin_stop_handler(
	Path(id): Path<String>,
	Query(query): Query<StopQuery>,
	State(state): State<AgentState>,
) -> AgentResult<Json<crate::plugin_application::Status>> {
	crate::plugin_application::stop_with(&state.plugins, &id, query.force)
		.await
		.map(Json)
		.map_err(bad_request)
}

/// A failed request to the agent, and whether trying again could succeed.
#[derive(Debug)]
struct RequestError {
	message: String,
	retryable: bool,
}

impl From<String> for RequestError {
	fn from(message: String) -> Self {
		Self { message, retryable: false }
	}
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
	options: crate::engine::execute::RunOptions,
) -> Result<crate::plugin_application::Status, String> {
	if options.fail_on_verdict {
		let capabilities: serde_json::Value =
			plugin_request(target, reqwest::Method::GET, "/plugins", None).await?;
		if capabilities["runOptions"]["failOnVerdict"] != true {
			return Err("remote agent does not support --fail-on-verdict; update the agent".into());
		}
	}
	plugin_request(
		target,
		reqwest::Method::POST,
		"/runs",
		Some(PluginRequest { xml, options, run_id: None }),
	)
	.await
}
pub async fn plugin_status(
	id: &str,
	target: &RemoteRunnerTarget,
) -> Result<crate::plugin_application::Status, String> {
	plugin_request(target, reqwest::Method::GET, &run_path(id, "")?, None).await
}
/// Retrieves a completed run's JUnit report for a local `--junit` copy.
pub async fn plugin_junit(id: &str, target: &RemoteRunnerTarget) -> Result<String, String> {
	plugin_request(target, reqwest::Method::GET, &run_path(id, "/junit")?, None).await
}
pub async fn plugin_stop(
	id: &str,
	target: &RemoteRunnerTarget,
) -> Result<crate::plugin_application::Status, String> {
	plugin_stop_with(id, target, false).await
}
/// Stops a remote run; with `force`, its teardown is interrupted too.
pub async fn plugin_stop_with(
	id: &str,
	target: &RemoteRunnerTarget,
	force: bool,
) -> Result<crate::plugin_application::Status, String> {
	let suffix = if force { "/stop?force=true" } else { "/stop" };
	plugin_request(target, reqwest::Method::POST, &run_path(id, suffix)?, None).await
}
pub async fn plugin_preflight(
	target: &RemoteRunnerTarget,
	xml: String,
) -> Result<serde_json::Value, String> {
	plugin_request(
		target,
		reqwest::Method::POST,
		"/preflight",
		Some(PluginRequest { xml, options: Default::default(), run_id: None }),
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
		Some(PluginRequest { xml, options: Default::default(), run_id: None }),
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

/// The shared HTTP client. It never follows redirects, so the bearer token only goes to the
/// validated endpoint, and every request has connect and total timeouts.
fn client() -> Result<&'static reqwest::Client, String> {
	static CLIENT: std::sync::OnceLock<Result<reqwest::Client, String>> =
		std::sync::OnceLock::new();
	CLIENT
		.get_or_init(|| {
			reqwest::Client::builder()
				.connect_timeout(CONNECT_TIMEOUT)
				.redirect(reqwest::redirect::Policy::none())
				.build()
				.map_err(|error| format!("could not create the remote runner client: {error}"))
		})
		.as_ref()
		.map_err(Clone::clone)
}

async fn plugin_request<T: serde::de::DeserializeOwned>(
	target: &RemoteRunnerTarget,
	method: reqwest::Method,
	path: &str,
	body: Option<PluginRequest>,
) -> Result<T, String> {
	request(target, method, path, body).await.map_err(|error| error.message)
}

async fn request<T: serde::de::DeserializeOwned>(
	target: &RemoteRunnerTarget,
	method: reqwest::Method,
	path: &str,
	body: Option<PluginRequest>,
) -> Result<T, RequestError> {
	// Inspect and preflight start plugins and run read-only steps, so they get longer.
	let timeout =
		if matches!(path, "/inspect" | "/preflight") { CHECK_TIMEOUT } else { REQUEST_TIMEOUT };
	request_within(target, method, path, body, timeout).await
}

/// One request to the agent that fails once `timeout` has passed, including the response body.
async fn request_within<T: serde::de::DeserializeOwned>(
	target: &RemoteRunnerTarget,
	method: reqwest::Method,
	path: &str,
	body: Option<PluginRequest>,
	timeout: Duration,
) -> Result<T, RequestError> {
	target.validate()?;
	let base = target.base_url()?;
	let mut request = client()?
		.request(method, format!("{base}{path}"))
		.bearer_auth(&target.bearer_token)
		.timeout(timeout);
	if let Some(body) = body {
		request = request.json(&body);
	}
	let response = request.send().await.map_err(|error| RequestError {
		message: if error.is_timeout() {
			format!("remote runner request timed out: {}", with_causes(&error))
		} else {
			format!("remote runner request failed: {}", with_causes(&error))
		},
		// The request may not have arrived; status polls and stops are safe to repeat.
		retryable: true,
	})?;
	decode_response(response).await
}

/// An error and its causes, such as `connection refused`, on one line.
fn with_causes(error: &dyn std::error::Error) -> String {
	let mut text = error.to_string();
	let mut cause = error.source();
	while let Some(error) = cause {
		let message = error.to_string();
		if !text.contains(&message) {
			text = format!("{text}: {message}");
		}
		cause = error.source();
	}
	text
}

/// Limits of a remote run as the client follows it.
#[derive(Clone, Debug)]
pub struct Follow {
	/// After the first signal, how long the agent gets to stop the run before the client forces
	/// the stop.
	pub grace: Duration,
	/// How long, from before the start request, the client follows the run at all. Every request
	/// and wait ends by then; the client then asks the agent to stop the run and gives up.
	pub deadline: Duration,
	/// How long start requests or status polls may keep failing before the client gives up.
	pub retry_budget: Duration,
	/// Time between status polls.
	pub poll_interval: Duration,
	/// Longest time one start or status request may take.
	pub request_timeout: Duration,
	/// Longest time one stop request may take.
	pub stop_timeout: Duration,
	/// After a forced stop, how long the client waits for the outcome before it stops waiting.
	pub force_limit: Duration,
}

impl Follow {
	/// Limits for a plan with this whole-run deadline and total teardown budget.
	pub fn for_plan(run: Duration, teardown: Duration, grace: Duration) -> Self {
		Self {
			grace,
			// Startup, report writing and plugin restarts fit in the extra minute.
			deadline: run + teardown + Duration::from_secs(60),
			retry_budget: RETRY_BUDGET,
			poll_interval: POLL_INTERVAL,
			request_timeout: REQUEST_TIMEOUT,
			stop_timeout: STOP_TIMEOUT,
			force_limit: FORCE_LIMIT,
		}
	}
}

/// How following a remote run ended.
#[derive(Debug)]
pub enum Followed {
	/// The run ended; its final status, and the first stop signal if one arrived.
	Ended { status: crate::plugin_application::Status, signal: Option<Signal> },
	/// The client stopped waiting after a forced stop, or after a signal before the run started.
	/// The run may still be stopping on the agent.
	Abandoned { id: String, signal: Signal },
}

/// What ended a wait.
enum Wake<T> {
	Done(T),
	Signal(Signal),
	Grace,
	ForceLimit,
	Deadline,
}

/// What a signal or timer asks the client to do.
enum Action {
	Nothing,
	Stop { force: bool },
	Abandon(Signal),
	Deadline,
}

/// A new client-chosen run ID. The agent starts at most one run per ID, so a start request whose
/// response was lost can be repeated, and the run can be stopped even if no response arrived.
fn new_run_id() -> String {
	let mut bytes = [0u8; 16];
	if getrandom::fill(&mut bytes).is_err() {
		// Unique enough without randomness: time, process and a counter.
		let now = std::time::SystemTime::now()
			.duration_since(std::time::UNIX_EPOCH)
			.unwrap_or_default()
			.as_nanos();
		bytes = (now ^ u128::from(std::process::id()) << 96).to_le_bytes();
	}
	format!("c-{}", hex::encode(bytes))
}

/// Starts a remote run and follows it to the end.
///
/// The client picks the run's ID and sends it with the start request; a start that fails in a
/// way that may succeed later is repeated with the same ID, for [`Follow::retry_budget`]. Status
/// polls that fail are retried with exponential backoff for the same budget. The first signal
/// asks the agent to stop the run and keeps following it; a second signal, or the end of
/// [`Follow::grace`], asks the agent to interrupt teardown too. Stop requests are short and are
/// themselves interrupted by signals and timers. Whenever the client gives up, it first asks the
/// agent to stop the run, so the agent does not stay busy.
pub async fn follow_run(
	target: &RemoteRunnerTarget,
	xml: String,
	options: crate::engine::execute::RunOptions,
	signals: &mut Signals,
	limits: &Follow,
) -> Result<Followed, String> {
	let mut follower = Follower {
		target,
		limits,
		deadline: Instant::now() + limits.deadline,
		id: new_run_id(),
		signals,
		first: None,
		grace_end: None,
		forced_at: None,
		forced_stop_sent: false,
	};
	follower.run(xml, options).await
}

/// The state of a followed remote run.
struct Follower<'a> {
	target: &'a RemoteRunnerTarget,
	limits: &'a Follow,
	/// Set before the start request; nothing waits past it.
	deadline: Instant,
	id: String,
	signals: &'a mut Signals,
	first: Option<Signal>,
	grace_end: Option<Instant>,
	forced_at: Option<Instant>,
	forced_stop_sent: bool,
}

impl Follower<'_> {
	async fn run(
		&mut self,
		xml: String,
		options: crate::engine::execute::RunOptions,
	) -> Result<Followed, String> {
		if let Some(followed) = self.start(xml, options).await? {
			return Ok(followed);
		}
		if let Some(first) = self.first {
			// A signal arrived while the run was being started.
			let force = self.forced_at.is_some();
			let _ = first;
			if let Some(followed) = self.act(Action::Stop { force }).await? {
				return Ok(followed);
			}
		}
		let path = run_path(&self.id, "")?;
		let mut failing_since: Option<Instant> = None;
		let mut delay = self.limits.poll_interval;
		loop {
			let timeout = self.request_timeout();
			let poll = request_within::<crate::plugin_application::Status>(
				self.target,
				reqwest::Method::GET,
				&path,
				None,
				timeout,
			);
			let result = match self.race(poll).await {
				Wake::Done(result) => result,
				other => {
					if let Some(followed) = self.on_wake(other).await? {
						return Ok(followed);
					}
					continue;
				},
			};
			match result {
				Ok(status) if status.state != "running" => {
					return Ok(Followed::Ended { status, signal: self.first });
				},
				Ok(_) => {
					failing_since = None;
					delay = self.limits.poll_interval;
				},
				Err(error) if !error.retryable => return Err(self.give_up(&error.message).await),
				Err(error) => {
					let since = *failing_since.get_or_insert_with(Instant::now);
					if since.elapsed() >= self.limits.retry_budget {
						let reason = format!(
							"status polls kept failing for {} s: {}",
							self.limits.retry_budget.as_secs(),
							error.message
						);
						return Err(self.give_up(&reason).await);
					}
					delay = (delay * 2).min(MAX_RETRY_DELAY);
					eprintln!(
						"{PRODUCT_NAME}: status poll failed ({}); retrying in {} ms",
						error.message,
						delay.as_millis()
					);
				},
			}
			if let Some(followed) = self.pause(delay).await? {
				return Ok(followed);
			}
		}
	}

	/// Sends the start request, repeating it with the same run ID while it fails in a way that
	/// may succeed later. Returns a final result only if following ends during the start.
	async fn start(
		&mut self,
		xml: String,
		options: crate::engine::execute::RunOptions,
	) -> Result<Option<Followed>, String> {
		let mut failing_since: Option<Instant> = None;
		let mut delay = self.limits.poll_interval;
		loop {
			let timeout = self.request_timeout();
			let request =
				PluginRequest { xml: xml.clone(), options, run_id: Some(self.id.clone()) };
			let start = start_within(self.target, request, timeout);
			let error = match self.race(start).await {
				Wake::Done(Ok(started)) => {
					// An agent that predates client-chosen IDs answers with its own.
					if is_safe_run_id(&started.id) {
						self.id = started.id;
					}
					return Ok(None);
				},
				Wake::Done(Err(error)) => error,
				Wake::Signal(signal) if self.first.is_none() => {
					// Let the start finish, so there is a run to stop; the signal is acted on next.
					self.note_first(signal);
					continue;
				},
				Wake::Signal(_) | Wake::Grace => {
					self.forced_at.get_or_insert_with(Instant::now);
					continue;
				},
				Wake::ForceLimit => {
					let signal = self.first.unwrap_or(Signal::Interrupt);
					let _ = self.stop_quietly(true).await;
					return Ok(Some(Followed::Abandoned { id: self.id.clone(), signal }));
				},
				Wake::Deadline => {
					return Err(self.give_up("the agent did not start the run in time").await);
				},
			};
			if let Some(signal) = self.first {
				// Stopping anyway: do not start again, but stop the run in case it started.
				let _ = self.stop_quietly(true).await;
				eprintln!("{PRODUCT_NAME}: the run was not started: {}", error.message);
				return Ok(Some(Followed::Abandoned { id: self.id.clone(), signal }));
			}
			if !error.retryable {
				return Err(self.give_up(&error.message).await);
			}
			let since = *failing_since.get_or_insert_with(Instant::now);
			if since.elapsed() >= self.limits.retry_budget {
				let reason = format!(
					"the start request kept failing for {} s: {}",
					self.limits.retry_budget.as_secs(),
					error.message
				);
				return Err(self.give_up(&reason).await);
			}
			delay = (delay * 2).min(MAX_RETRY_DELAY);
			eprintln!(
				"{PRODUCT_NAME}: start request failed ({}); retrying run {} in {} ms",
				error.message,
				self.id,
				delay.as_millis()
			);
			if let Some(followed) = self.pause(delay).await? {
				return Ok(Some(followed));
			}
		}
	}

	/// Waits `delay` unless a signal or timer needs handling first.
	async fn pause(&mut self, delay: Duration) -> Result<Option<Followed>, String> {
		match self.race(tokio::time::sleep(delay)).await {
			Wake::Done(()) => Ok(None),
			other => self.on_wake(other).await,
		}
	}

	/// Waits for `work`, the next signal (until the stop is forced), the end of the grace
	/// period, the end of a forced stop, or the client's deadline.
	async fn race<F: std::future::Future>(&mut self, work: F) -> Wake<F::Output> {
		let grace_end = self.grace_end.filter(|_| self.forced_at.is_none());
		let force_end = self.forced_at.map(|at| at + self.limits.force_limit);
		let until = |at: Option<Instant>| async move {
			match at {
				Some(at) => tokio::time::sleep_until(at).await,
				None => std::future::pending().await,
			}
		};
		// The deadline comes first: a request cut short by it reports the deadline, not a timeout.
		tokio::select! {
			biased;
			() = tokio::time::sleep_until(self.deadline) => Wake::Deadline,
			output = work => Wake::Done(output),
			signal = self.signals.recv(), if force_end.is_none() => Wake::Signal(signal),
			() = until(grace_end) => Wake::Grace,
			() = until(force_end) => Wake::ForceLimit,
		}
	}

	fn note_first(&mut self, signal: Signal) {
		self.first = Some(signal);
		self.grace_end = Some(Instant::now() + self.limits.grace);
		eprintln!(
			"{PRODUCT_NAME}: received {}; asking the agent to stop the run (teardown has up to {} s; send the signal again to stop now)",
			signal.name(),
			self.limits.grace.as_secs()
		);
	}

	/// What a signal or timer means for the run.
	fn action<T>(&mut self, woke: Wake<T>) -> Action {
		match woke {
			Wake::Done(_) => Action::Nothing,
			Wake::Signal(signal) if self.first.is_none() => {
				self.note_first(signal);
				Action::Stop { force: false }
			},
			Wake::Signal(_) | Wake::Grace => {
				if self.forced_at.is_some() {
					return Action::Nothing;
				}
				self.forced_at = Some(Instant::now());
				eprintln!("{PRODUCT_NAME}: asking the agent to stop now; teardown is interrupted");
				Action::Stop { force: true }
			},
			Wake::ForceLimit => Action::Abandon(self.first.unwrap_or(Signal::Interrupt)),
			Wake::Deadline => Action::Deadline,
		}
	}

	async fn on_wake<T>(&mut self, woke: Wake<T>) -> Result<Option<Followed>, String> {
		let action = self.action(woke);
		self.act(action).await
	}

	async fn act(&mut self, action: Action) -> Result<Option<Followed>, String> {
		match action {
			Action::Nothing => Ok(None),
			Action::Stop { force } => self.stop(force).await,
			Action::Abandon(signal) => {
				Ok(Some(Followed::Abandoned { id: self.id.clone(), signal }))
			},
			Action::Deadline => {
				let reason = format!(
					"the run did not end within the client's deadline of {} s",
					self.limits.deadline.as_secs()
				);
				Err(self.give_up(&reason).await)
			},
		}
	}

	/// Asks the agent to stop the run. The request is short and gives way to a second signal,
	/// which sends the forced stop at once, and to the force limit and deadline.
	async fn stop(&mut self, force: bool) -> Result<Option<Followed>, String> {
		let mut force = force;
		loop {
			if force && self.forced_stop_sent {
				return Ok(None);
			}
			let timeout = self.stop_timeout();
			let id = self.id.clone();
			let send = stop_within(self.target, &id, force, timeout);
			match self.race(send).await {
				Wake::Done(result) => {
					if let Err(error) = result {
						eprintln!(
							"{PRODUCT_NAME}: could not ask the agent to stop run {}: {}",
							self.id, error.message
						);
					}
					self.forced_stop_sent |= force;
					return Ok(None);
				},
				other => match self.action(other) {
					Action::Nothing => {},
					Action::Stop { force: escalate } => force |= escalate,
					action => return Box::pin(self.act(action)).await,
				},
			}
		}
	}

	/// One stop request without waiting for signals, bounded by [`Follow::stop_timeout`].
	async fn stop_quietly(&self, force: bool) -> Result<(), String> {
		stop_within(self.target, &self.id, force, self.limits.stop_timeout)
			.await
			.map(|_| ())
			.map_err(|error| error.message)
	}

	/// Asks the agent to stop the run before the client gives up, and says whether it could.
	async fn give_up(&self, reason: &str) -> String {
		match self.stop_quietly(false).await {
			Ok(()) => format!("{reason}; asked the agent to stop run {}", self.id),
			Err(error) => {
				format!("{reason}; could not ask the agent to stop run {}: {error}", self.id)
			},
		}
	}

	/// What is left of `limit` before the client's deadline, and the force limit once forced.
	fn remaining(&self, limit: Duration) -> Duration {
		let now = Instant::now();
		let mut end = self.deadline;
		if let Some(at) = self.forced_at {
			end = end.min(at + self.limits.force_limit);
		}
		limit.min(end.saturating_duration_since(now)).max(Duration::from_millis(1))
	}

	fn request_timeout(&self) -> Duration {
		self.remaining(self.limits.request_timeout)
	}

	fn stop_timeout(&self) -> Duration {
		self.remaining(self.limits.stop_timeout)
	}
}

/// The start request: the capability check for the run options, then `POST /runs`.
async fn start_within(
	target: &RemoteRunnerTarget,
	request: PluginRequest,
	timeout: Duration,
) -> Result<crate::plugin_application::Status, RequestError> {
	let started = Instant::now();
	if request.options.fail_on_verdict {
		let capabilities: serde_json::Value =
			request_within(target, reqwest::Method::GET, "/plugins", None, timeout).await?;
		if capabilities["runOptions"]["failOnVerdict"] != true {
			return Err("remote agent does not support --fail-on-verdict; update the agent"
				.to_owned()
				.into());
		}
	}
	let left = timeout.saturating_sub(started.elapsed()).max(Duration::from_millis(1));
	request_within(target, reqwest::Method::POST, "/runs", Some(request), left).await
}

async fn stop_within(
	target: &RemoteRunnerTarget,
	id: &str,
	force: bool,
	timeout: Duration,
) -> Result<crate::plugin_application::Status, RequestError> {
	let suffix = if force { "/stop?force=true" } else { "/stop" };
	request_within(target, reqwest::Method::POST, &run_path(id, suffix)?, None, timeout).await
}

#[cfg(test)]
mod tests {
	use super::*;
	use crate::plugin_application::Status;
	use std::sync::{
		Mutex,
		atomic::{AtomicUsize, Ordering},
	};
	use tokio::sync::Notify;

	#[test]
	fn remote_run_options_default_for_older_clients_and_round_trip() {
		let request: PluginRequest =
			serde_json::from_value(serde_json::json!({"xml":"plan"})).unwrap();
		assert!(!request.options.fail_on_verdict);
		let request = PluginRequest {
			xml: "plan".into(),
			options: crate::engine::execute::RunOptions { fail_on_verdict: true },
			run_id: Some("c-1".into()),
		};
		let json = serde_json::to_value(request).unwrap();
		assert_eq!(json["options"]["failOnVerdict"], true);
		assert_eq!(json["runId"], "c-1");
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
			crate::engine::execute::RunOptions { fail_on_verdict: true },
		)
		.await
		else {
			panic!("older agent accepted the policy")
		};
		assert_eq!(error, "remote agent does not support --fail-on-verdict; update the agent");
		server.abort();
	}

	fn target(endpoint: &str) -> RemoteRunnerTarget {
		RemoteRunnerTarget { endpoint: endpoint.into(), bearer_token: "token".into() }
	}

	#[test]
	fn remote_targets_require_tls_or_a_loopback_tunnel() {
		for endpoint in [
			"http://127.0.0.1:9901",
			"http://127.0.0.2:9901/",
			"http://localhost:9901",
			"http://LOCALHOST",
			"http://[::1]:9901",
			"https://runner.example",
			"https://runner.example:8443/agent/",
		] {
			assert!(target(endpoint).validate().is_ok(), "{endpoint} was refused");
		}
		for endpoint in [
			"http://runner.example",
			"http://127.0.0.1.nip.io:9901",
			"http://localhost.runner.example:9901",
			"http://[::2]:9901",
			"ftp://127.0.0.1:9901",
			"127.0.0.1:9901",
			"http://127.0.0.1:9901/?next=x",
			"http://127.0.0.1:9901/#x",
			"",
		] {
			assert!(target(endpoint).validate().is_err(), "{endpoint} was accepted");
		}
		assert_eq!(
			target("https://runner.example:8443/agent/").base_url().unwrap(),
			"https://runner.example:8443/agent"
		);
		assert_eq!(target("http://127.1:9901").base_url().unwrap(), "http://127.0.0.1:9901");
	}

	#[test]
	fn user_information_cannot_hide_the_real_host() {
		for endpoint in [
			"http://127.0.0.1:9901@example.invalid:9901",
			"http://localhost:80@evil.example:80",
			"http://127.0.0.1:1@192.168.1.10:8766",
			"https://user:password@runner.example",
			"https://user@runner.example",
		] {
			assert_eq!(
				target(endpoint).validate().unwrap_err(),
				"remote runner endpoint must not contain user information (user@ or user:password@)",
				"{endpoint}"
			);
		}
	}

	async fn serve_on_loopback(app: Router) -> (String, tokio::task::JoinHandle<()>) {
		let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
		let endpoint = format!("http://{}", listener.local_addr().unwrap());
		let server = tokio::spawn(async move {
			axum::serve(listener, app).await.unwrap();
		});
		(endpoint, server)
	}

	#[tokio::test]
	async fn redirects_are_not_followed_so_the_token_stays_with_the_endpoint() {
		let other_hits = Arc::new(AtomicUsize::new(0));
		let hits = other_hits.clone();
		let (other, other_server) = serve_on_loopback(Router::new().fallback(move || {
			hits.fetch_add(1, Ordering::SeqCst);
			async { Json(serde_json::json!({"id":"x","state":"completed"})) }
		}))
		.await;
		let location = format!("{other}/runs/run-1");
		let (endpoint, server) = serve_on_loopback(Router::new().fallback(move || {
			let location = location.clone();
			async move {
				(StatusCode::TEMPORARY_REDIRECT, [(axum::http::header::LOCATION, location)], "")
			}
		}))
		.await;
		let error = plugin_status("run-1", &target(&endpoint)).await.unwrap_err();
		assert!(error.starts_with("remote runner returned 307 Temporary Redirect"), "{error}");
		assert_eq!(other_hits.load(Ordering::SeqCst), 0);
		server.abort();
		other_server.abort();
	}

	/// What the mock agent answers to the n-th status poll.
	#[derive(Clone, Copy, Debug)]
	enum Poll {
		Fail(u16),
		Running,
		/// Never answers.
		Stall,
		Completed,
	}

	/// What the mock agent answers to the n-th start request.
	#[derive(Clone, Copy, Debug)]
	enum Start {
		Running,
		Fail(u16),
		/// Never answers, like a proxy that lost the response.
		Stall,
	}

	#[derive(Default)]
	struct Mock {
		script: Vec<Poll>,
		starts: Vec<Start>,
		polls: AtomicUsize,
		/// The run IDs the start requests carried.
		start_ids: Mutex<Vec<Option<String>>>,
		/// The run IDs the status polls asked for.
		poll_ids: Mutex<Vec<String>>,
		/// The stop requests received, as `stop` or `force`, with the run ID.
		stops: Mutex<Vec<(&'static str, String)>>,
		/// Notified when a poll stalls or a start request arrives.
		stalled: Notify,
		started: Notify,
		/// When set, the start request waits for this.
		start_gate: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
		/// Keep answering from the script after a stop.
		ignore_stops: bool,
		/// Stop requests without `force` never get an answer.
		stall_plain_stops: bool,
	}

	impl Mock {
		fn stop_kinds(&self) -> Vec<&'static str> {
			self.stops.lock().unwrap().iter().map(|(kind, _)| *kind).collect()
		}
	}

	fn status(state: &str, exit_code: Option<i32>) -> Status {
		Status {
			id: "run-1".into(),
			state: state.into(),
			outcome: exit_code.map(|exit_code| crate::engine::execute::Outcome {
				artifact_dir: "/agent/runs/run-1".into(),
				exit_code,
				state: if exit_code == 0 {
					crate::engine::execute::RunState::Completed
				} else {
					crate::engine::execute::RunState::Stopped
				},
				error: None,
				teardown: None,
			}),
			..Status::default()
		}
	}

	async fn mock_agent(mock: Arc<Mock>) -> (String, tokio::task::JoinHandle<()>) {
		let (start, poll, stop) = (mock.clone(), mock.clone(), mock);
		let app = Router::new()
			.route(
				"/runs",
				post(move |Json(request): Json<PluginRequest>| {
					let mock = start.clone();
					async move {
						let index = {
							let mut ids = mock.start_ids.lock().unwrap();
							ids.push(request.run_id.clone());
							ids.len() - 1
						};
						mock.started.notify_one();
						let gate = mock.start_gate.lock().unwrap().take();
						if let Some(gate) = gate {
							let _ = gate.await;
						}
						match mock.starts.get(index).copied().unwrap_or(Start::Running) {
							Start::Running => {
								// Like a current agent, the run takes the ID the client chose.
								let id = request.run_id.unwrap_or_else(|| "run-1".into());
								Json(Status { id, ..status("running", None) }).into_response()
							},
							Start::Fail(code) => {
								(StatusCode::from_u16(code).unwrap(), "agent trouble")
									.into_response()
							},
							Start::Stall => std::future::pending().await,
						}
					}
				}),
			)
			.route(
				"/runs/{id}",
				get(move |Path(id): Path<String>| {
					let mock = poll.clone();
					async move {
						mock.poll_ids.lock().unwrap().push(id);
						let index = mock.polls.fetch_add(1, Ordering::SeqCst);
						if !mock.ignore_stops && !mock.stops.lock().unwrap().is_empty() {
							return Json(status("stopped", Some(130))).into_response();
						}
						match mock.script.get(index).or(mock.script.last()).copied() {
							Some(Poll::Fail(code)) => {
								(StatusCode::from_u16(code).unwrap(), "agent trouble")
									.into_response()
							},
							Some(Poll::Stall) => {
								mock.stalled.notify_one();
								std::future::pending().await
							},
							Some(Poll::Completed) => {
								Json(status("completed", Some(0))).into_response()
							},
							Some(Poll::Running) | None => {
								Json(status("running", None)).into_response()
							},
						}
					}
				}),
			)
			.route(
				"/runs/{id}/stop",
				post(move |Path(id): Path<String>, Query(query): Query<StopQuery>| {
					let mock = stop.clone();
					async move {
						let kind = if query.force { "force" } else { "stop" };
						mock.stops.lock().unwrap().push((kind, id));
						if !query.force && mock.stall_plain_stops {
							mock.stalled.notify_one();
							return std::future::pending().await;
						}
						Json(status("running", None)).into_response()
					}
				}),
			);
		serve_on_loopback(app).await
	}

	fn limits() -> Follow {
		Follow {
			grace: Duration::from_secs(30),
			deadline: Duration::from_secs(60),
			retry_budget: Duration::from_secs(30),
			poll_interval: Duration::from_millis(1),
			request_timeout: Duration::from_secs(30),
			stop_timeout: Duration::from_secs(30),
			force_limit: Duration::from_secs(30),
		}
	}

	async fn follow(
		endpoint: &str,
		signals: &mut Signals,
		limits: &Follow,
	) -> Result<Followed, String> {
		follow_run(&target(endpoint), "plan".into(), Default::default(), signals, limits).await
	}

	#[tokio::test]
	async fn failed_status_polls_are_retried_until_the_run_ends() {
		let mock = Arc::new(Mock {
			script: vec![
				Poll::Fail(503),
				Poll::Fail(502),
				Poll::Running,
				Poll::Fail(500),
				Poll::Completed,
			],
			..Mock::default()
		});
		let (endpoint, server) = mock_agent(mock.clone()).await;
		let (_sender, mut signals) = Signals::channel();
		let Followed::Ended { status, signal } =
			follow(&endpoint, &mut signals, &limits()).await.unwrap()
		else {
			panic!("the run was abandoned")
		};
		assert_eq!((status.state.as_str(), signal), ("completed", None));
		assert_eq!(mock.polls.load(Ordering::SeqCst), 5);
		assert!(mock.stops.lock().unwrap().is_empty());
		// Every poll asks for the run ID the client chose and sent with the start.
		let start_ids = mock.start_ids.lock().unwrap().clone();
		let [Some(id)] = start_ids.as_slice() else { panic!("{start_ids:?}") };
		assert!(id.starts_with("c-") && is_safe_run_id(id), "{id}");
		assert!(mock.poll_ids.lock().unwrap().iter().all(|polled| polled == id));
		server.abort();
	}

	#[tokio::test]
	async fn a_lost_start_response_is_repeated_with_the_same_run_id() {
		let mock = Arc::new(Mock {
			starts: vec![Start::Stall, Start::Fail(502), Start::Running],
			script: vec![Poll::Completed],
			..Mock::default()
		});
		let (endpoint, server) = mock_agent(mock.clone()).await;
		let (_sender, mut signals) = Signals::channel();
		let limits = Follow { request_timeout: Duration::from_millis(200), ..limits() };
		let Followed::Ended { status, .. } =
			follow(&endpoint, &mut signals, &limits).await.unwrap()
		else {
			panic!("the run was abandoned")
		};
		assert_eq!(status.state, "completed");
		let ids = mock.start_ids.lock().unwrap().clone();
		assert_eq!(ids.len(), 3);
		assert!(ids.iter().all(|id| id.is_some() && *id == ids[0]), "{ids:?}");
		server.abort();
	}

	#[tokio::test]
	async fn a_start_that_keeps_failing_stops_the_run_by_its_id() {
		let mock = Arc::new(Mock { starts: vec![Start::Fail(503); 100], ..Mock::default() });
		let (endpoint, server) = mock_agent(mock.clone()).await;
		let (_sender, mut signals) = Signals::channel();
		let limits = Follow { retry_budget: Duration::ZERO, ..limits() };
		let error = follow(&endpoint, &mut signals, &limits).await.unwrap_err();
		assert!(error.starts_with("the start request kept failing for 0 s"), "{error}");
		let id = mock.start_ids.lock().unwrap()[0].clone().unwrap();
		assert!(error.ends_with(&format!("; asked the agent to stop run {id}")), "{error}");
		assert_eq!(*mock.stops.lock().unwrap(), [("stop", id)]);
		server.abort();
	}

	#[tokio::test]
	async fn giving_up_on_failed_polls_stops_the_remote_run() {
		let mock = Arc::new(Mock { script: vec![Poll::Fail(503)], ..Mock::default() });
		let (endpoint, server) = mock_agent(mock.clone()).await;
		let (_sender, mut signals) = Signals::channel();
		let limits = Follow { retry_budget: Duration::ZERO, ..limits() };
		let error = follow(&endpoint, &mut signals, &limits).await.unwrap_err();
		assert!(
			error.starts_with("status polls kept failing for 0 s: remote runner returned 503"),
			"{error}"
		);
		assert!(error.contains("; asked the agent to stop run c-"), "{error}");
		assert_eq!(mock.stop_kinds(), ["stop"]);
		server.abort();
	}

	#[tokio::test]
	async fn an_unrecoverable_poll_error_stops_the_remote_run_at_once() {
		let mock = Arc::new(Mock { script: vec![Poll::Fail(400)], ..Mock::default() });
		let (endpoint, server) = mock_agent(mock.clone()).await;
		let (_sender, mut signals) = Signals::channel();
		let error = follow(&endpoint, &mut signals, &limits()).await.unwrap_err();
		assert!(
			error.starts_with(
				"remote runner returned 400 Bad Request: agent trouble; asked the agent to stop run c-"
			),
			"{error}"
		);
		assert_eq!(mock.polls.load(Ordering::SeqCst), 1);
		assert_eq!(mock.stop_kinds(), ["stop"]);
		server.abort();
	}

	#[tokio::test]
	async fn the_client_deadline_bounds_a_stalled_poll_and_stops_the_run() {
		let mock = Arc::new(Mock { script: vec![Poll::Stall], ..Mock::default() });
		let (endpoint, server) = mock_agent(mock.clone()).await;
		let (_sender, mut signals) = Signals::channel();
		// The poll itself could wait 30 s; the deadline ends it.
		let limits = Follow { deadline: Duration::from_millis(300), ..limits() };
		let started = Instant::now();
		let error = follow(&endpoint, &mut signals, &limits).await.unwrap_err();
		assert!(started.elapsed() < Duration::from_secs(10));
		assert!(
			error.starts_with(
				"the run did not end within the client's deadline of 0 s; asked the agent to stop run c-"
			),
			"{error}"
		);
		assert_eq!(mock.stop_kinds(), ["stop"]);
		server.abort();
	}

	#[tokio::test]
	async fn the_client_deadline_also_bounds_the_start() {
		let mock = Arc::new(Mock { starts: vec![Start::Stall], ..Mock::default() });
		let (endpoint, server) = mock_agent(mock.clone()).await;
		let (_sender, mut signals) = Signals::channel();
		let limits = Follow { deadline: Duration::from_millis(200), ..limits() };
		let error = follow(&endpoint, &mut signals, &limits).await.unwrap_err();
		assert!(error.starts_with("the agent did not start the run in time; asked"), "{error}");
		assert_eq!(mock.stop_kinds(), ["stop"]);
		server.abort();
	}

	#[tokio::test]
	async fn a_signal_during_a_stalled_poll_stops_the_remote_run() {
		let mock = Arc::new(Mock { script: vec![Poll::Running, Poll::Stall], ..Mock::default() });
		let (endpoint, server) = mock_agent(mock.clone()).await;
		let (sender, mut signals) = Signals::channel();
		let stalled = mock.clone();
		tokio::spawn(async move {
			stalled.stalled.notified().await;
			sender.send(Signal::Terminate).unwrap();
		});
		let Followed::Ended { status, signal } =
			follow(&endpoint, &mut signals, &limits()).await.unwrap()
		else {
			panic!("the run was abandoned")
		};
		assert_eq!((status.state.as_str(), signal), ("stopped", Some(Signal::Terminate)));
		assert_eq!(mock.stop_kinds(), ["stop"]);
		server.abort();
	}

	#[tokio::test]
	async fn a_signal_during_the_start_request_stops_the_run_once_it_started() {
		let (release, gate) = tokio::sync::oneshot::channel();
		let mock = Arc::new(Mock {
			script: vec![Poll::Running],
			start_gate: Mutex::new(Some(gate)),
			..Mock::default()
		});
		let (endpoint, server) = mock_agent(mock.clone()).await;
		let (sender, mut signals) = Signals::channel();
		let started = mock.clone();
		tokio::spawn(async move {
			started.started.notified().await;
			sender.send(Signal::Interrupt).unwrap();
			// The client has the signal before the agent answers.
			tokio::task::yield_now().await;
			let _ = release.send(());
			// Keep the channel open, as the operating system does.
			std::future::pending::<()>().await;
		});
		let Followed::Ended { status, signal } =
			follow(&endpoint, &mut signals, &limits()).await.unwrap()
		else {
			panic!("the run was abandoned")
		};
		assert_eq!((status.state.as_str(), signal), ("stopped", Some(Signal::Interrupt)));
		assert_eq!(mock.stop_kinds(), ["stop"]);
		server.abort();
	}

	#[tokio::test]
	async fn a_second_signal_forces_the_stop_even_while_the_first_stop_hangs() {
		// The plain stop never answers, and the agent never reports the end of the run.
		let mock = Arc::new(Mock {
			script: vec![Poll::Running],
			ignore_stops: true,
			stall_plain_stops: true,
			..Mock::default()
		});
		let (endpoint, server) = mock_agent(mock.clone()).await;
		let (sender, mut signals) = Signals::channel();
		let stalled = mock.clone();
		let first = sender.clone();
		tokio::spawn(async move {
			// The first signal, once the client is polling.
			while stalled.polls.load(Ordering::SeqCst) == 0 {
				tokio::task::yield_now().await;
			}
			first.send(Signal::Terminate).unwrap();
			// The second, once the plain stop request is hanging.
			stalled.stalled.notified().await;
			sender.send(Signal::Terminate).unwrap();
			std::future::pending::<()>().await;
		});
		let limits = Follow { force_limit: Duration::from_millis(300), ..limits() };
		let started = Instant::now();
		let followed = follow(&endpoint, &mut signals, &limits).await.unwrap();
		let Followed::Abandoned { id, signal } = followed else { panic!("the run ended") };
		assert!(id.starts_with("c-"));
		assert_eq!(signal, Signal::Terminate);
		assert_eq!(mock.stop_kinds(), ["stop", "force"]);
		// Neither the 30 s stop timeout nor the 30 s grace period was waited for.
		assert!(started.elapsed() < Duration::from_secs(10), "{:?}", started.elapsed());
		server.abort();
	}

	#[tokio::test]
	async fn a_stalled_request_times_out_and_can_be_retried() {
		let (endpoint, server) = serve_on_loopback(
			Router::new().fallback(std::future::pending::<Json<serde_json::Value>>),
		)
		.await;
		let started = std::time::Instant::now();
		let error = request_within::<Status>(
			&target(&endpoint),
			reqwest::Method::GET,
			"/runs/run-1",
			None,
			Duration::from_millis(100),
		)
		.await
		.unwrap_err();
		assert!(error.message.starts_with("remote runner request timed out"), "{error:?}");
		assert!(error.retryable);
		assert!(started.elapsed() < Duration::from_secs(10));
		server.abort();
	}

	#[test]
	fn tokens_must_be_long_and_varied() {
		let message = "must have at least 32 characters, at least 8 of them different; generate one with `openssl rand -hex 32`";
		for token in ["x".repeat(MIN_TOKEN_CHARS - 1), "a".repeat(64), "abababab".repeat(8)] {
			assert_eq!(check_token(&token).unwrap_err(), message, "{token}");
		}
		assert!(check_token("0123456789abcdef0123456789abcdef").is_ok());
	}

	#[tokio::test]
	async fn the_agent_refuses_weak_tokens() {
		let (_sender, mut signals) = Signals::channel();
		let settings = AgentSettings {
			output_root: "unused".into(),
			keep_runs: None,
			grace: Duration::from_secs(1),
		};
		let error = serve("127.0.0.1:0", "a".repeat(MIN_TOKEN_CHARS), settings, &mut signals)
			.await
			.unwrap_err();
		assert!(error.starts_with("agent bearer token must have at least 32 characters"));
	}

	const TOKEN: &str = "0123456789abcdef0123456789abcdef";

	#[tokio::test]
	async fn a_stopping_agent_refuses_new_runs() {
		let plugins = Arc::new(crate::plugin_application::State::default());
		plugins.closing.cancel();
		let state = AgentState {
			bearer_token: TOKEN.into(),
			output_root: "unused".into(),
			plugins,
			delivered: Arc::default(),
		};
		let (endpoint, server) = serve_on_loopback(router(state)).await;
		let error = plugin_start(
			&RemoteRunnerTarget { endpoint, bearer_token: TOKEN.into() },
			"plan".into(),
		)
		.await
		.unwrap_err();
		assert_eq!(
			error,
			"remote runner returned 503 Service Unavailable: the agent is shutting down"
		);
		server.abort();
	}

	#[tokio::test]
	async fn the_agent_starts_one_run_per_client_run_id() {
		let root = std::env::temp_dir().join(format!(
			"polkameter-idempotent-{}-{}",
			std::process::id(),
			new_run_id()
		));
		let plugins = Arc::new(crate::plugin_application::State::default());
		let state = AgentState {
			bearer_token: TOKEN.into(),
			output_root: root.to_string_lossy().into(),
			plugins: plugins.clone(),
			delivered: Arc::default(),
		};
		let (endpoint, server) = serve_on_loopback(router(state)).await;
		let target = RemoteRunnerTarget { endpoint, bearer_token: TOKEN.into() };
		let xml = r#"<polkameter-plan xmlns="https://polkameter.dev/schema/plan" version="1" name="once"><setup><step id="echo" use="core.echo"><input name="value" value="1"/></step></setup></polkameter-plan>"#;
		let request = || PluginRequest {
			xml: xml.into(),
			options: Default::default(),
			run_id: Some("c-idempotent".into()),
		};
		let first: Status = request_within(
			&target,
			reqwest::Method::POST,
			"/runs",
			Some(request()),
			REQUEST_TIMEOUT,
		)
		.await
		.unwrap();
		assert_eq!(first.id, "c-idempotent");
		crate::plugin_application::finished(&plugins).await;
		let again: Status = request_within(
			&target,
			reqwest::Method::POST,
			"/runs",
			Some(request()),
			REQUEST_TIMEOUT,
		)
		.await
		.unwrap();
		assert_eq!((again.id.as_str(), again.state.as_str()), ("c-idempotent", "completed"));
		assert_eq!(std::fs::read_dir(&root).unwrap().count(), 1, "a second run started");
		let bad = PluginRequest { run_id: Some("../x".into()), ..request() };
		let error = request_within::<Status>(
			&target,
			reqwest::Method::POST,
			"/runs",
			Some(bad),
			REQUEST_TIMEOUT,
		)
		.await
		.unwrap_err();
		assert_eq!(error.message, "remote runner returned 400 Bad Request: invalid run ID");
		server.abort();
		std::fs::remove_dir_all(root).unwrap();
	}

	#[test]
	fn run_ids_cannot_be_path_segments_or_separators() {
		for id in ["c-0a1b", "79007-00000000000000000001", "a.b_c"] {
			assert!(is_safe_run_id(id), "{id}");
		}
		for id in ["", ".", "..", "../x", "a/b", "a\\b", "a?b", "a%2Fb", &"x".repeat(129)] {
			assert!(!is_safe_run_id(id), "{id}");
		}
	}
}
