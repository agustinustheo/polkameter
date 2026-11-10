//! Portable JMeter-compatible samples, written from a run's raw files. A sample is a measured
//! time: a transaction PolkaMeter has no time for has no row (`transactions.jsonl` has them all).
use anyhow::Result;
use serde::Serialize;
use serde_json::Value;
use std::{
	fs,
	io::{BufRead as _, BufReader},
	path::Path,
};

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

/// The records of a JSON-lines file, read one line at a time.
fn records(path: &Path) -> Result<impl Iterator<Item = Result<Value>>> {
	let lines = BufReader::new(fs::File::open(path)?).lines();
	Ok(lines.map(|line| Ok(serde_json::from_str(&line?)?)))
}

/// samples.jtl: one row per step and per transaction in the run's event and ledger files, which
/// are read one line at a time. As in JMeter, `timeStamp` is when a sample started and `elapsed`
/// how long it took: a step's start is its finish time minus its elapsed time.
pub fn write_samples(directory: &Path) -> Result<()> {
	let mut out = csv::WriterBuilder::new()
		.has_headers(false)
		.from_path(directory.join("samples.jtl"))?;
	out.write_record(HEADER)?;
	for event in records(&directory.join("events.jsonl"))? {
		let event = event?;
		if event["event"] != "step-finished" {
			continue;
		}
		let success = event["success"] == true;
		let elapsed = event["elapsedMs"].as_u64().unwrap_or(0);
		out.serialize(Sample {
			time_stamp: event["timestamp"].as_u64().unwrap_or(0).saturating_sub(elapsed),
			elapsed,
			label: format!("step:{}", event["step"].as_str().unwrap_or("unknown")),
			response_code: if success { "STEP_OK" } else { "STEP_FAILED" }.into(),
			response_message: event["error"].as_str().unwrap_or("").into(),
			thread_name: "steps".into(),
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
		for tx in records(&ledger)? {
			let tx = tx?;
			let success = tx["status"] == "finalized" && tx["failedInFinalizedBlock"] == false;
			// JMeter reads every row's elapsed and Latency as numbers, so a tx without both
			// times measured has no row: no 0 or reply-time stand-in for a time not measured.
			// elapsed is the time to the best block that included it, or to the node's refusal.
			let reply = tx["replyMs"].as_u64();
			let elapsed =
				if tx["status"] == "refused" { reply } else { tx["inclusionMs"].as_u64() };
			let (Some(elapsed), Some(reply), false) = (elapsed, reply, tx["untimed"] == true)
			else {
				continue;
			};
			out.serialize(Sample {
				time_stamp: tx["sentAt"].as_u64().unwrap_or(0),
				elapsed,
				label: format!("load:step:{}", tx["step"]),
				response_code: tx["status"].as_str().unwrap_or("unknown").into(),
				response_message: tx["hash"].as_str().unwrap_or("").into(),
				thread_name: "load".into(),
				success,
				bytes: 0,
				sent_bytes: 0,
				latency: reply,
				connect: 0,
				all_threads: 1,
				grp_threads: 1,
			})?;
		}
	}
	out.flush()?;
	Ok(())
}
