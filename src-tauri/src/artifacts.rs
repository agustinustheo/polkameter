use std::{
	fs,
	path::{Path, PathBuf},
	time::{SystemTime, UNIX_EPOCH},
};

use serde::{Deserialize, Serialize};

use crate::scenario::{ResolvedPlan, ScenarioDocument};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SampleRecord {
	#[serde(rename = "timeStamp")]
	pub timestamp: u64,
	pub elapsed: u64,
	pub label: String,
	#[serde(rename = "responseCode")]
	pub response_code: String,
	#[serde(rename = "responseMessage")]
	pub response_message: String,
	#[serde(rename = "threadName")]
	pub thread_name: String,
	pub success: bool,
	pub bytes: u64,
	#[serde(rename = "sentBytes")]
	pub sent_bytes: u64,
	#[serde(rename = "Latency")]
	pub latency: u64,
	#[serde(rename = "Connect")]
	pub connect: u64,
	#[serde(rename = "allThreads")]
	pub all_threads: u32,
	#[serde(rename = "grpThreads")]
	pub group_threads: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct EventRecord {
	pub ts: String,
	pub label: String,
	pub account: String,
	pub sampler_phase: String,
	pub scheduled_ms: u64,
	pub submit_ms: Option<u64>,
	pub completed_ms: Option<u64>,
	pub extrinsic_hash: Option<String>,
	pub block_hash: Option<String>,
	pub outcome: String,
	pub message: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct TelemetryRecord {
	pub ts: String,
	pub elapsed_ms: u64,
	pub completed_samples: u64,
	pub successful_samples: u64,
	pub failed_samples: u64,
	pub timed_out_samples: u64,
	pub cpu_percent: f64,
	pub rss_kib: u64,
	pub process_tree_rss_kib: u64,
	pub best_block: Option<u64>,
	pub finalized_block: Option<u64>,
	pub pending_extrinsics: Option<u64>,
	pub rpc_error: Option<String>,
	pub node_cpu_seconds_total: Option<f64>,
	pub node_rss_kib: Option<u64>,
	pub node_ready_transactions: Option<u64>,
	pub prometheus_error: Option<String>,
}

pub struct ArtifactWriter {
	pub directory: PathBuf,
	samples: csv::Writer<fs::File>,
	events: fs::File,
}

impl ArtifactWriter {
	pub fn create(
		output_root: impl AsRef<Path>,
		scenario: &ScenarioDocument,
		run_id: &str,
	) -> Result<Self, String> {
		let directory = output_root.as_ref().join(run_id);
		fs::create_dir_all(directory.join("plots")).map_err(|error| error.to_string())?;
		write_json(directory.join("scenario.polkameter.json"), &scenario.redacted_clone())?;
		let resolved_plan = ResolvedPlan::from_scenario(scenario, run_id);
		write_json(directory.join("resolved-plan.json"), &resolved_plan)?;
		write_json(directory.join("config.json"), &resolved_plan)?;
		fs::write(
			directory.join("command.txt"),
			"Polkameter run started through the desktop application\n",
		)
		.map_err(|error| error.to_string())?;

		let samples_path = directory.join("samples.jtl");
		let samples = csv::Writer::from_path(&samples_path).map_err(|error| error.to_string())?;

		let events =
			fs::File::create(directory.join("events.jsonl")).map_err(|error| error.to_string())?;
		fs::File::create(directory.join("telemetry.jsonl")).map_err(|error| error.to_string())?;
		Ok(Self { directory, samples, events })
	}

	pub fn write_sample(&mut self, sample: &SampleRecord) -> Result<(), String> {
		self.samples.serialize(sample).map_err(|error| error.to_string())?;
		self.samples.flush().map_err(|error| error.to_string())
	}

	pub fn write_event(&mut self, event: &EventRecord) -> Result<(), String> {
		write_json_line(&mut self.events, event)
	}

	pub fn flush(&mut self) -> Result<(), String> {
		use std::io::Write;
		self.samples.flush().map_err(|error| error.to_string())?;
		self.events.flush().map_err(|error| error.to_string())
	}

	pub fn write_summary(&self, markdown: &str) -> Result<(), String> {
		fs::write(self.directory.join("summary.md"), markdown).map_err(|error| error.to_string())
	}
}

fn write_json(path: impl AsRef<Path>, value: &impl Serialize) -> Result<(), String> {
	let encoded = serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?;
	fs::write(path, encoded).map_err(|error| error.to_string())
}

fn write_json_line(file: &mut fs::File, value: &impl Serialize) -> Result<(), String> {
	use std::io::Write;
	serde_json::to_writer(&mut *file, value).map_err(|error| error.to_string())?;
	file.write_all(b"\n").map_err(|error| error.to_string())?;
	file.flush().map_err(|error| error.to_string())
}

pub fn new_run_id() -> String {
	let timestamp = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis();
	format!("run-{timestamp}")
}

#[cfg(test)]
pub(crate) fn test_scenario() -> ScenarioDocument {
	use crate::scenario::{
		ArrivalModel, ChainTarget, Collector, CompletionBoundary, DevSignerSource, RunLimits,
		SamplerPhase, TestPlan, ThreadGroup, TransactionProfile, TransactionSampler,
		SCENARIO_VERSION,
	};

	ScenarioDocument {
		version: SCENARIO_VERSION,
		test_plan: TestPlan {
			name: "test".into(),
			description: String::new(),
			seed: 1,
			limits: RunLimits::default(),
		},
		chain: ChainTarget {
			endpoint: "ws://127.0.0.1:9944".into(),
			prometheus_endpoint: None,
			transaction_profile: TransactionProfile::Polkadot,
		},
		signer_source: DevSignerSource {
			profile: "local-dev".into(),
			base_suri: "//Alice".into(),
			derivation_path: "//polkameter".into(),
			funding: None,
		},
		thread_groups: vec![ThreadGroup {
			name: "users".into(),
			users: 1,
			concurrency: 1,
			iterations: 1,
			arrival: ArrivalModel::Burst { window_ms: 1 },
			samplers: vec![TransactionSampler {
				phase: SamplerPhase::Transaction,
				label: "call".into(),
				pallet: "Balances".into(),
				call: "transfer_keep_alive".into(),
				arguments: serde_json::json!({}),
				completion: CompletionBoundary::Finalized,
				mortality_period: 4,
				finality_timeout_ms: 1_000,
				assertions: vec![],
			}],
		}],
		collectors: vec![Collector::Jtl],
	}
}

#[cfg(test)]
mod tests {
	use std::fs;

	use super::*;

	#[test]
	fn writer_creates_portable_artifacts_without_secrets() {
		let root = std::env::temp_dir().join(format!("polkameter-artifact-test-{}", new_run_id()));
		let mut writer =
			ArtifactWriter::create(&root, &test_scenario(), "proof").expect("writer created");
		writer.write_summary("# Run\n").expect("summary written");
		writer.flush().expect("artifacts flushed");
		let plan =
			fs::read_to_string(writer.directory.join("resolved-plan.json")).expect("plan readable");
		let native = fs::read_to_string(writer.directory.join("scenario.polkameter.json"))
			.expect("scenario readable");
		assert!(!plan.contains("//Alice"));
		assert!(!native.contains("//Alice"));
		assert!(writer.directory.join("samples.jtl").is_file());
		assert!(writer.directory.join("config.json").is_file());
		let _ = fs::remove_dir_all(root);
	}
}
