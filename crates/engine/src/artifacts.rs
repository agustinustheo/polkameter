//! Portable JMeter-compatible samples alongside the detailed reconciliation ledger.
use anyhow::Result;
use serde_json::Value;
use std::{fs, path::Path};

pub fn write_samples(directory: &Path) -> Result<()> {
	let mut out = csv::Writer::from_path(directory.join("samples.jtl"))?;
	out.write_record([
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
	])?;
	let events = fs::read_to_string(directory.join("events.jsonl"))?;
	for line in events.lines() {
		let event: Value = serde_json::from_str(line)?;
		if event["event"] != "step-finished" {
			continue;
		}
		let success = event["success"] == true;
		out.write_record([
			event["timestamp"].as_u64().unwrap_or(0).to_string(),
			event["elapsedMs"].as_u64().unwrap_or(0).to_string(),
			format!("step:{}", event["step"].as_str().unwrap_or("unknown")),
			if success { "STEP_OK" } else { "STEP_FAILED" }.into(),
			event["error"].as_str().unwrap_or("").into(),
			format!("user:{}:iteration:{}", event["user"], event["iteration"]),
			success.to_string(),
			"0".into(),
			"0".into(),
			"0".into(),
			"0".into(),
			"1".into(),
			"1".into(),
		])?;
	}
	let ledger = directory.join("transactions.jsonl");
	if ledger.exists() {
		for line in fs::read_to_string(ledger)?.lines() {
			let tx: Value = serde_json::from_str(line)?;
			let success = tx["status"] == "finalized" && tx["failedInFinalizedBlock"] == false;
			out.write_record([
				tx["sentAt"].as_u64().unwrap_or(0).to_string(),
				tx["inclusionMs"]
					.as_u64()
					.or_else(|| tx["replyMs"].as_u64())
					.unwrap_or(0)
					.to_string(),
				format!("load:step:{}", tx["step"]),
				tx["status"].as_str().unwrap_or("unknown").into(),
				tx["hash"].as_str().unwrap_or("").into(),
				"load".into(),
				success.to_string(),
				"0".into(),
				"0".into(),
				tx["replyMs"].as_u64().unwrap_or(0).to_string(),
				"0".into(),
				"1".into(),
				"1".into(),
			])?;
		}
	}
	out.flush()?;
	// v2 node.jsonl/scrapes.jsonl remain authoritative; no fabricated v1 observations.

	Ok(())
}
