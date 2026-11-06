//! Portable JMeter-compatible samples and plots, written from a run's raw files.
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

/// What every finished run, and `polkameter report`, derives from the raw files: `samples.jtl`
/// and the plots.
pub fn write_outputs(directory: &Path) -> Result<()> {
	write_samples(directory)?;
	crate::engine::plots::write(directory)
}

/// The records of a JSON-lines file, read one line at a time.
fn records(path: &Path) -> Result<impl Iterator<Item = Result<Value>>> {
	let lines = BufReader::new(fs::File::open(path)?).lines();
	Ok(lines.map(|line| Ok(serde_json::from_str(&line?)?)))
}

/// samples.jtl: one row per step and per transaction in the run's event and ledger files, which
/// are read one line at a time. As in JMeter, `timeStamp` is when a sample started and `elapsed`
/// how long it took: a step's start is its finish time minus its elapsed time.
fn write_samples(directory: &Path) -> Result<()> {
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
		for tx in records(&ledger)? {
			let tx = tx?;
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

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn samples_are_jmeter_csv_stamped_with_their_start() {
		let dir = std::env::temp_dir().join(format!("polkameter-jtl-{}", std::process::id()));
		fs::create_dir_all(&dir).unwrap();
		let message = "refused, \"bad\"\nsecond line";
		let events = [
			serde_json::json!({"event":"step-started","step":"a","timestamp":1000}),
			serde_json::json!({"event":"step-finished","step":"a","user":0,"iteration":1,
				"success":false,"error":message,"elapsedMs":9000,"timestamp":10000}),
		];
		let text: String = events.iter().map(|e| format!("{e}\n")).collect();
		fs::write(dir.join("events.jsonl"), text).unwrap();
		write_samples(&dir).unwrap();
		let jtl = fs::read_to_string(dir.join("samples.jtl")).unwrap();
		assert!(jtl.starts_with(&format!("{}\n", HEADER.join(","))));
		// RFC 4180 quoting, which JMeter's CSV reader also reads: quotes doubled, field quoted.
		assert!(jtl.contains("\"refused, \"\"bad\"\"\nsecond line\""), "{jtl}");
		let mut reader = csv::Reader::from_path(dir.join("samples.jtl")).unwrap();
		assert_eq!(reader.headers().unwrap(), HEADER.as_slice());
		let rows: Vec<csv::StringRecord> = reader.records().map(Result::unwrap).collect();
		assert_eq!(rows.len(), 1);
		// timeStamp is the start (finish 10000 minus 9000 elapsed), as JMeter expects.
		assert_eq!(&rows[0][0], "1000");
		assert_eq!(&rows[0][1], "9000");
		assert_eq!(&rows[0][2], "step:a");
		assert_eq!(&rows[0][3], "STEP_FAILED");
		assert_eq!(&rows[0][4], message);
		assert_eq!(&rows[0][5], "user:0:iteration:1");
		assert_eq!(&rows[0][6], "false");
		fs::remove_dir_all(&dir).unwrap();
	}
}
