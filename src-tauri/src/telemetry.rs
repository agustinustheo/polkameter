use std::{
	fs::OpenOptions,
	io::{BufWriter, Write},
	path::Path,
	process::Command,
	sync::Arc,
	time::Duration,
};

use tokio::{sync::watch, task::JoinHandle};

use crate::{
	artifacts::TelemetryRecord,
	runner::{status, RunnerState},
};

pub struct TelemetryHandle {
	stop: watch::Sender<bool>,
	handle: JoinHandle<Result<(), String>>,
}

impl TelemetryHandle {
	pub async fn stop(self) -> Result<(), String> {
		let _ = self.stop.send(true);
		self.handle.await.map_err(|error| error.to_string())?
	}
}

pub fn spawn(
	run_dir: &Path,
	endpoint: String,
	prometheus_endpoint: Option<String>,
	started_ms: u64,
	state: Arc<RunnerState>,
) -> Result<TelemetryHandle, String> {
	let path = run_dir.join("telemetry.jsonl");
	let (stop, mut stop_rx) = watch::channel(false);
	let handle = tokio::spawn(async move {
		let file = OpenOptions::new()
			.create(true)
			.truncate(true)
			.write(true)
			.open(path)
			.map_err(|error| error.to_string())?;
		let mut writer = BufWriter::new(file);
		loop {
			write_record(
				&mut writer,
				&endpoint,
				prometheus_endpoint.as_deref(),
				started_ms,
				state.clone(),
			)
			.await?;
			tokio::select! {
				_ = tokio::time::sleep(Duration::from_secs(5)) => {},
				changed = stop_rx.changed() => {
					if changed.is_ok() && *stop_rx.borrow() {
					write_record(&mut writer, &endpoint, prometheus_endpoint.as_deref(), started_ms, state.clone()).await?;
						break;
					}
				},
			}
		}
		Ok(())
	});
	Ok(TelemetryHandle { stop, handle })
}

async fn write_record(
	writer: &mut BufWriter<std::fs::File>,
	endpoint: &str,
	prometheus_endpoint: Option<&str>,
	started_ms: u64,
	state: Arc<RunnerState>,
) -> Result<(), String> {
	let snapshot = status(state).await;
	let process = process_metrics();
	let chain = chain_metrics(endpoint);
	let node = prometheus_metrics(prometheus_endpoint);
	let record = TelemetryRecord {
		ts: now_ms().to_string(),
		elapsed_ms: now_ms().saturating_sub(started_ms),
		completed_samples: snapshot.completed_samples,
		successful_samples: snapshot.successful_samples,
		failed_samples: snapshot.failed_samples,
		timed_out_samples: snapshot.timed_out_samples,
		cpu_percent: process.cpu_percent,
		rss_kib: process.rss_kib,
		process_tree_rss_kib: process.process_tree_rss_kib,
		best_block: chain.best_block,
		finalized_block: chain.finalized_block,
		pending_extrinsics: chain.pending_extrinsics,
		rpc_error: chain.rpc_error,
		node_cpu_seconds_total: node.cpu_seconds_total,
		node_rss_kib: node.rss_kib,
		node_ready_transactions: node.ready_transactions,
		prometheus_error: node.error,
	};
	serde_json::to_writer(&mut *writer, &record).map_err(|error| error.to_string())?;
	writer.write_all(b"\n").map_err(|error| error.to_string())?;
	writer.flush().map_err(|error| error.to_string())
}

#[derive(Default)]
struct NodeMetrics {
	cpu_seconds_total: Option<f64>,
	rss_kib: Option<u64>,
	ready_transactions: Option<u64>,
	error: Option<String>,
}

fn prometheus_metrics(endpoint: Option<&str>) -> NodeMetrics {
	let Some(endpoint) = endpoint else { return NodeMetrics::default() };
	let output = Command::new("curl").args(["-sS", "-m", "2", endpoint]).output();
	let Ok(output) = output else {
		return NodeMetrics {
			error: Some("could not start Prometheus request".into()),
			..NodeMetrics::default()
		};
	};
	if !output.status.success() {
		return NodeMetrics {
			error: Some(format!("Prometheus request exited with {}", output.status)),
			..NodeMetrics::default()
		};
	}
	let body = String::from_utf8_lossy(&output.stdout);
	let rss_kib =
		metric_value(&body, "process_resident_memory_bytes").map(|bytes| (bytes / 1024.0) as u64);
	let ready_transactions =
		["substrate_ready_transactions_number", "substrate_transaction_pool_ready"]
			.into_iter()
			.find_map(|name| metric_value(&body, name))
			.map(|value| value as u64);
	NodeMetrics {
		cpu_seconds_total: metric_value(&body, "process_cpu_seconds_total"),
		rss_kib,
		ready_transactions,
		error: None,
	}
}

fn metric_value(body: &str, target: &str) -> Option<f64> {
	body.lines().filter(|line| !line.starts_with('#')).find_map(|line| {
		let mut fields = line.split_whitespace();
		let name = fields.next()?.split('{').next()?;
		(name == target).then(|| fields.next()?.parse().ok()).flatten()
	})
}

#[derive(Default)]
struct ProcessMetrics {
	cpu_percent: f64,
	rss_kib: u64,
	process_tree_rss_kib: u64,
}

fn process_metrics() -> ProcessMetrics {
	let pid = std::process::id();
	let output = Command::new("ps")
		.args(["-o", "%cpu=", "-o", "rss=", "-p", &pid.to_string()])
		.output();
	let Ok(output) = output else {
		return ProcessMetrics::default();
	};
	let line = String::from_utf8_lossy(&output.stdout);
	let fields = line.split_whitespace().collect::<Vec<_>>();
	let [cpu, rss] = fields.as_slice() else {
		return ProcessMetrics::default();
	};
	ProcessMetrics {
		cpu_percent: cpu.parse().unwrap_or(0.0),
		rss_kib: rss.parse().unwrap_or(0),
		process_tree_rss_kib: rss.parse().unwrap_or(0),
	}
}

#[derive(Default)]
struct ChainMetrics {
	best_block: Option<u64>,
	finalized_block: Option<u64>,
	pending_extrinsics: Option<u64>,
	rpc_error: Option<String>,
}

fn chain_metrics(endpoint: &str) -> ChainMetrics {
	let Some(url) = endpoint.strip_prefix("ws://").map(|value| format!("http://{value}")) else {
		return ChainMetrics {
			rpc_error: Some("telemetry currently supports ws:// RPC endpoints".into()),
			..ChainMetrics::default()
		};
	};
	let best = rpc(&url, "chain_getHeader", serde_json::json!([]));
	let finalized = rpc(&url, "chain_getFinalizedHead", serde_json::json!([]))
		.and_then(|hash| rpc(&url, "chain_getHeader", serde_json::json!([hash])));
	let pending = rpc(&url, "author_pendingExtrinsics", serde_json::json!([]));
	let mut errors = Vec::new();
	let best_block = match best {
		Ok(value) => block_number(&value),
		Err(error) => {
			errors.push(error);
			None
		},
	};
	let finalized_block = match finalized {
		Ok(value) => block_number(&value),
		Err(error) => {
			errors.push(error);
			None
		},
	};
	let pending_extrinsics = match pending {
		Ok(value) => value.as_array().map(|items| items.len() as u64),
		Err(error) => {
			errors.push(error);
			None
		},
	};
	ChainMetrics {
		best_block,
		finalized_block,
		pending_extrinsics,
		rpc_error: (!errors.is_empty()).then(|| errors.join("; ")),
	}
}

fn rpc(url: &str, method: &str, params: serde_json::Value) -> Result<serde_json::Value, String> {
	let body = serde_json::json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params })
		.to_string();
	let output = Command::new("curl")
		.args(["-sS", "-m", "2", "-H", "Content-Type: application/json", "-d", &body, url])
		.output()
		.map_err(|error| format!("{method}: {error}"))?;
	let response = serde_json::from_slice::<serde_json::Value>(&output.stdout)
		.map_err(|error| format!("{method}: {error}"))?;
	response
		.get("result")
		.cloned()
		.ok_or_else(|| format!("{method}: {}", response.get("error").unwrap_or(&response)))
}

fn block_number(value: &serde_json::Value) -> Option<u64> {
	value
		.get("number")?
		.as_str()?
		.strip_prefix("0x")
		.and_then(|number| u64::from_str_radix(number, 16).ok())
}

fn now_ms() -> u64 {
	use std::time::{SystemTime, UNIX_EPOCH};
	SystemTime::now()
		.duration_since(UNIX_EPOCH)
		.unwrap_or_default()
		.as_millis()
		.try_into()
		.unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn parses_prometheus_metrics_with_labels() {
		let metrics = "# HELP process_resident_memory_bytes resident memory\nprocess_resident_memory_bytes 4194304\nprocess_cpu_seconds_total 3.5\nsubstrate_ready_transactions_number{chain=\"dev\"} 7\n";
		assert_eq!(metric_value(metrics, "process_resident_memory_bytes"), Some(4_194_304.0));
		assert_eq!(metric_value(metrics, "process_cpu_seconds_total"), Some(3.5));
		assert_eq!(metric_value(metrics, "substrate_ready_transactions_number"), Some(7.0));
	}
}
