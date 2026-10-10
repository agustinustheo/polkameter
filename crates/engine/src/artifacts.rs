//! Portable JMeter-compatible samples and plots, written from a run's raw files.
use anyhow::Result;
use serde::Serialize;
use serde_json::Value;
use std::{fs, path::Path};

/// Column names of samples.jtl, in the order of `Sample`'s fields.
const HEADER: [&str; 13] = [
	"timeStamp",
	"elapsed",
	"label",
	"responseCode",
	"responseMessage",
	"threadName",
	"success",
	"bytes",
	"sentBytes",
	"Latency",
	"Connect",
	"allThreads",
	"grpThreads",
];

/// One samples.jtl row. The header is written once by hand, so the writer must not add its own.
#[derive(Serialize)]
struct Sample {
	time_stamp: u64,
	elapsed: u64,
	label: String,
	response_code: String,
	response_message: String,
	thread_name: String,
	success: bool,
	bytes: u64,
	sent_bytes: u64,
	latency: u64,
	connect: u64,
	all_threads: u64,
	grp_threads: u64,
}

/// What every finished run, and `polkameter report`, derives from the raw files: `samples.jtl`
/// and the plots.
pub fn write_outputs(directory: &Path) -> Result<()> {
	write_samples(directory)?;
	crate::plots::write(directory)
}

/// samples.jtl: one row per step and per transaction in the run's event and ledger files.
fn write_samples(directory: &Path) -> Result<()> {
	let mut out = csv::WriterBuilder::new()
		.has_headers(false)
		.from_path(directory.join("samples.jtl"))?;
	out.write_record(HEADER)?;
	for line in fs::read_to_string(directory.join("events.jsonl"))?.lines() {
		let event: Value = serde_json::from_str(line)?;
		if event["event"] != "step-finished" {
			continue;
		}
		let success = event["success"] == true;
		out.serialize(Sample {
			time_stamp: event["timestamp"].as_u64().unwrap_or(0),
			elapsed: event["elapsedMs"].as_u64().unwrap_or(0),
			label: format!("step:{}", event["step"].as_str().unwrap_or("unknown")),
			response_code: if success { "STEP_OK" } else { "STEP_FAILED" }.into(),
			response_message: event["error"].as_str().unwrap_or("").into(),
			thread_name: format!("user:{}:iteration:{}", event["user"], event["iteration"]),
			success,
			bytes: 0,
			sent_bytes: 0,
			latency: 0,
			connect: 0,
			all_threads: 1,
			grp_threads: 1,
		})?;
	}
	let ledger = directory.join("transactions.jsonl");
	if ledger.exists() {
		for line in fs::read_to_string(ledger)?.lines() {
			let tx: Value = serde_json::from_str(line)?;
			let success = tx["status"] == "finalized" && tx["failedInFinalizedBlock"] == false;
			out.serialize(Sample {
				time_stamp: tx["sentAt"].as_u64().unwrap_or(0),
				elapsed: tx["inclusionMs"].as_u64().or_else(|| tx["replyMs"].as_u64()).unwrap_or(0),
				label: format!("load:step:{}", tx["step"]),
				response_code: tx["status"].as_str().unwrap_or("unknown").into(),
				response_message: tx["hash"].as_str().unwrap_or("").into(),
				thread_name: "load".into(),
				success,
				bytes: 0,
				sent_bytes: 0,
				latency: tx["replyMs"].as_u64().unwrap_or(0),
				connect: 0,
				all_threads: 1,
				grp_threads: 1,
			})?;
		}
	}
	out.flush()?;
	Ok(())
}
