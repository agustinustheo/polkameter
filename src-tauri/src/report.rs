use std::{collections::BTreeMap, fs, path::Path};

use serde::{Deserialize, Serialize};

use crate::artifacts::{SampleRecord, TelemetryRecord};

pub struct Report {
	pub summary: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardReport {
	pub summary: String,
	pub plots: Vec<DashboardPlot>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DashboardPlot {
	pub name: String,
	pub svg: String,
}

pub fn write(run_dir: &Path) -> Result<Report, String> {
	let samples = read_samples(run_dir)?;
	let telemetry = read_telemetry(run_dir)?;
	let plots = run_dir.join("plots");
	fs::create_dir_all(&plots).map_err(|error| error.to_string())?;
	fs::write(plots.join("throughput.svg"), throughput_svg(&samples))
		.map_err(|error| error.to_string())?;
	fs::write(plots.join("latency-percentiles.svg"), latency_svg(&samples))
		.map_err(|error| error.to_string())?;
	fs::write(plots.join("failure-breakdown.svg"), failures_svg(&samples))
		.map_err(|error| error.to_string())?;
	fs::write(plots.join("cpu-memory.svg"), cpu_memory_svg(&telemetry))
		.map_err(|error| error.to_string())?;
	fs::write(plots.join("blocks-pending.svg"), blocks_svg(&telemetry))
		.map_err(|error| error.to_string())?;
	fs::write(plots.join("node-resources.svg"), node_resources_svg(&telemetry))
		.map_err(|error| error.to_string())?;
	Ok(Report { summary: summary_markdown(&samples, &telemetry) })
}

pub fn validate(run_dir: &Path) -> Result<(), String> {
	for name in [
		"scenario.polkameter.json",
		"resolved-plan.json",
		"config.json",
		"command.txt",
		"samples.jtl",
		"events.jsonl",
		"telemetry.jsonl",
		"summary.md",
	] {
		let path = run_dir.join(name);
		if !path.is_file() {
			return Err(format!("missing artifact {}", path.display()));
		}
	}
	for name in [
		"throughput",
		"latency-percentiles",
		"failure-breakdown",
		"cpu-memory",
		"blocks-pending",
		"node-resources",
	] {
		let path = run_dir.join("plots").join(format!("{name}.svg"));
		if fs::metadata(&path).map_err(|error| error.to_string())?.len() < 80 {
			return Err(format!("missing or empty plot {}", path.display()));
		}
	}
	for name in ["scenario.polkameter.json", "resolved-plan.json", "config.json"] {
		let text = fs::read_to_string(run_dir.join(name)).map_err(|error| error.to_string())?;
		if text.contains("//Alice") || text.contains("//Bob") {
			return Err(format!("secret-like material leaked into {name}"));
		}
		let document = serde_json::from_str::<serde_json::Value>(&text)
			.map_err(|error| format!("invalid artifact {name}: {error}"))?;
		if !has_redacted_signer_source(&document) {
			return Err(format!("unredacted signer source in {name}"));
		}
	}
	Ok(())
}

fn has_redacted_signer_source(value: &serde_json::Value) -> bool {
	let signer = value
		.get("signerSource")
		.or_else(|| value.get("scenario").and_then(|scenario| scenario.get("signerSource")));
	signer
		.and_then(|source| source.get("baseSuri"))
		.and_then(serde_json::Value::as_str)
		== Some("[redacted]")
}

pub fn read_dashboard(run_dir: &Path) -> Result<DashboardReport, String> {
	validate(run_dir)?;
	let summary =
		fs::read_to_string(run_dir.join("summary.md")).map_err(|error| error.to_string())?;
	let plots = [
		"throughput",
		"latency-percentiles",
		"failure-breakdown",
		"cpu-memory",
		"blocks-pending",
		"node-resources",
	]
	.into_iter()
	.map(|name| {
		let svg = fs::read_to_string(run_dir.join("plots").join(format!("{name}.svg")))
			.map_err(|error| error.to_string())?;
		Ok(DashboardPlot { name: name.into(), svg })
	})
	.collect::<Result<Vec<_>, String>>()?;
	Ok(DashboardReport { summary, plots })
}

fn read_samples(run_dir: &Path) -> Result<Vec<SampleRecord>, String> {
	let mut reader =
		csv::Reader::from_path(run_dir.join("samples.jtl")).map_err(|error| error.to_string())?;
	reader
		.deserialize()
		.collect::<Result<Vec<_>, _>>()
		.map_err(|error| error.to_string())
}

fn read_telemetry(run_dir: &Path) -> Result<Vec<TelemetryRecord>, String> {
	let text =
		fs::read_to_string(run_dir.join("telemetry.jsonl")).map_err(|error| error.to_string())?;
	text.lines()
		.filter(|line| !line.trim().is_empty())
		.map(serde_json::from_str)
		.collect::<Result<Vec<_>, _>>()
		.map_err(|error| error.to_string())
}

fn summary_markdown(samples: &[SampleRecord], telemetry: &[TelemetryRecord]) -> String {
	let total = samples.len();
	let success = samples.iter().filter(|sample| sample.success).count();
	let failed = total.saturating_sub(success);
	let timed_out =
		samples.iter().filter(|sample| sample.response_code.contains("TIMEOUT")).count();
	let elapsed = samples.iter().map(|sample| sample.elapsed).max().unwrap_or_default();
	let mut latencies = samples.iter().map(|sample| sample.elapsed).collect::<Vec<_>>();
	latencies.sort_unstable();
	let max_cpu = telemetry.iter().map(|record| record.cpu_percent).fold(0.0, f64::max);
	let max_rss = telemetry.iter().map(|record| record.rss_kib).max().unwrap_or_default();
	let max_pending = telemetry
		.iter()
		.filter_map(|record| record.pending_extrinsics)
		.max()
		.unwrap_or_default();
	let max_node_rss = telemetry.iter().filter_map(|record| record.node_rss_kib).max();
	let max_node_ready = telemetry.iter().filter_map(|record| record.node_ready_transactions).max();
	let max_node_cpu = telemetry
		.windows(2)
		.filter_map(|records| {
			let elapsed = records[1].elapsed_ms.saturating_sub(records[0].elapsed_ms);
			let cpu = records[1].node_cpu_seconds_total? - records[0].node_cpu_seconds_total?;
			(elapsed > 0 && cpu >= 0.0).then_some(cpu * 100_000.0 / elapsed as f64)
		})
		.fold(0.0, f64::max);
	let final_best = telemetry
		.last()
		.and_then(|record| record.best_block)
		.map_or_else(|| "n/a".into(), |block| block.to_string());
	let final_finalized = telemetry
		.last()
		.and_then(|record| record.finalized_block)
		.map_or_else(|| "n/a".into(), |block| block.to_string());
	let mut node_rows = String::new();
	if let Some(rss) = max_node_rss {
		node_rows.push_str(&format!(
			"| Node max RSS | {rss} KiB |\n| Node max CPU | {max_node_cpu:.1}% |\n"
		));
	}
	if let Some(ready) = max_node_ready {
		node_rows.push_str(&format!("| Node max ready transactions | {ready} |\n"));
	}
	format!("# Polkameter Run\n\n| Metric | Result |\n|---|---:|\n| Samples | {total} |\n| Successful | {success} |\n| Failed | {failed} |\n| Timed out | {timed_out} |\n| Max sample elapsed | {elapsed} ms |\n| Latency p50 | {} ms |\n| Latency p95 | {} ms |\n| Latency p99 | {} ms |\n| Max CPU | {max_cpu:.1}% |\n| Max RSS | {max_rss} KiB |\n| Max pending extrinsics | {max_pending} |\n{node_rows}| Final best block | {final_best} |\n| Final finalized block | {final_finalized} |\n", percentile(&latencies, 50), percentile(&latencies, 95), percentile(&latencies, 99))
}

fn percentile(values: &[u64], percentile: usize) -> u64 {
	if values.is_empty() {
		return 0;
	}
	values[((values.len() - 1) * percentile / 100).min(values.len() - 1)]
}

fn throughput_svg(samples: &[SampleRecord]) -> String {
	let first = samples.iter().map(|sample| sample.timestamp).min().unwrap_or_default();
	let mut buckets = BTreeMap::<u64, u64>::new();
	for sample in samples {
		*buckets.entry(sample.timestamp.saturating_sub(first) / 1_000).or_default() += 1;
	}
	chart("Throughput", "samples / second", buckets.into_iter().collect(), "#2d9d8b")
}

fn latency_svg(samples: &[SampleRecord]) -> String {
	let mut values = samples.iter().map(|sample| sample.elapsed).collect::<Vec<_>>();
	values.sort_unstable();
	chart(
		"Latency percentiles",
		"milliseconds",
		vec![
			(50, percentile(&values, 50)),
			(95, percentile(&values, 95)),
			(99, percentile(&values, 99)),
		],
		"#486fb5",
	)
}

fn failures_svg(samples: &[SampleRecord]) -> String {
	let mut counts = BTreeMap::<String, u64>::new();
	for sample in samples {
		if !sample.success {
			*counts.entry(sample.response_code.clone()).or_default() += 1;
		}
	}
	let points = counts
		.into_iter()
		.enumerate()
		.map(|(index, (_, value))| (index as u64 + 1, value))
		.collect();
	chart("Failure breakdown", "failed samples", points, "#c95c4d")
}

fn cpu_memory_svg(telemetry: &[TelemetryRecord]) -> String {
	let first = telemetry.first().map(|record| record.elapsed_ms).unwrap_or_default();
	let points = telemetry
		.iter()
		.map(|record| (record.elapsed_ms.saturating_sub(first) / 1_000, record.rss_kib))
		.collect();
	chart("CPU and memory", "RSS KiB", points, "#9a6bb1")
}

fn blocks_svg(telemetry: &[TelemetryRecord]) -> String {
	let first = telemetry.first().map(|record| record.elapsed_ms).unwrap_or_default();
	let points = telemetry
		.iter()
		.map(|record| {
			(
				record.elapsed_ms.saturating_sub(first) / 1_000,
				record.pending_extrinsics.unwrap_or_default(),
			)
		})
		.collect();
	chart("Blocks and pending extrinsics", "pending extrinsics", points, "#ce8f38")
}

fn node_resources_svg(telemetry: &[TelemetryRecord]) -> String {
	let first = telemetry.first().map(|record| record.elapsed_ms).unwrap_or_default();
	let rss_points: Vec<_> = telemetry
		.iter()
		.filter_map(|record| {
			record
				.node_rss_kib
				.map(|rss| (record.elapsed_ms.saturating_sub(first) / 1_000, rss))
		})
		.collect();
	if !rss_points.is_empty() {
		return chart("Node resources", "node RSS KiB", rss_points, "#b65b89");
	}
	let ready_points: Vec<_> = telemetry
		.iter()
		.filter_map(|record| {
			record
				.node_ready_transactions
				.map(|ready| (record.elapsed_ms.saturating_sub(first) / 1_000, ready))
		})
		.collect();
	chart("Node resources", "node ready transactions", ready_points, "#b65b89")
}

fn chart(title: &str, unit: &str, points: Vec<(u64, u64)>, color: &str) -> String {
	let left = 58.0;
	let bottom = 270.0;
	let max_x = points.iter().map(|point| point.0).max().unwrap_or(1).max(1) as f64;
	let max_y = points.iter().map(|point| point.1).max().unwrap_or(1).max(1) as f64;
	let path = points
		.iter()
		.enumerate()
		.map(|(index, (x, y))| {
			format!(
				"{} {:.1},{:.1}",
				if index == 0 { "M" } else { "L" },
				left + *x as f64 / max_x * 860.0,
				bottom - *y as f64 / max_y * 190.0
			)
		})
		.collect::<Vec<_>>()
		.join(" ");
	format!("<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"960\" height=\"320\" viewBox=\"0 0 960 320\"><rect width=\"960\" height=\"320\" fill=\"#f8fafb\"/><text x=\"32\" y=\"38\" fill=\"#263744\" font-family=\"sans-serif\" font-size=\"20\" font-weight=\"700\">{title}</text><text x=\"32\" y=\"62\" fill=\"#667784\" font-family=\"sans-serif\" font-size=\"12\">{unit}</text><path d=\"M {left} 80 V {bottom} H 918\" fill=\"none\" stroke=\"#cdd8de\"/><path d=\"{path}\" fill=\"none\" stroke=\"{color}\" stroke-width=\"3\"/><text x=\"32\" y=\"274\" fill=\"#667784\" font-family=\"sans-serif\" font-size=\"11\">0</text><text x=\"32\" y=\"94\" fill=\"#667784\" font-family=\"sans-serif\" font-size=\"11\">{max_y:.0}</text></svg>")
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn percentile_uses_nearest_observed_value() {
		assert_eq!(percentile(&[1, 5, 9, 100], 95), 9);
	}

	#[test]
	fn chart_contains_real_data_path() {
		assert!(chart("Test", "units", vec![(0, 1), (1, 3)], "#000").contains("L"));
	}

	#[test]
	fn artifact_secret_check_allows_hex_call_arguments() {
		assert!(has_redacted_signer_source(&serde_json::json!({
			"signerSource": { "baseSuri": "[redacted]" },
			"testPlan": { "seed": 1 },
			"arguments": { "$bytes": "0x0102" }
		})));
		assert!(!has_redacted_signer_source(&serde_json::json!({
			"signerSource": { "baseSuri": "//Alice" }
		})));
	}

	#[test]
	fn report_writes_all_run_plots_from_empty_collectors() {
		let directory =
			std::env::temp_dir().join(format!("polkameter-report-test-{}", std::process::id()));
		let _ = fs::remove_dir_all(&directory);
		fs::create_dir_all(&directory).expect("directory created");
		fs::write(&directory.join("samples.jtl"), "timeStamp,elapsed,label,responseCode,responseMessage,threadName,success,bytes,sentBytes,Latency,Connect,allThreads,grpThreads\n").expect("samples written");
		fs::write(&directory.join("telemetry.jsonl"), "").expect("telemetry written");
		let report = write(&directory).expect("report created");
		assert!(report.summary.contains("Samples | 0"));
		assert!(directory.join("plots/throughput.svg").is_file());
		let _ = fs::remove_dir_all(directory);
	}

	#[test]
	fn dashboard_reads_a_validated_artifact_bundle() {
		let directory =
			std::env::temp_dir().join(format!("polkameter-dashboard-test-{}", std::process::id()));
		let _ = fs::remove_dir_all(&directory);
		let scenario = crate::artifacts::test_scenario();
		let mut writer =
			crate::artifacts::ArtifactWriter::create(&directory, &scenario, "proof", "test\n")
				.expect("artifact bundle created");
		writer.flush().expect("collectors flushed");
		let report = write(&writer.directory).expect("report written");
		writer.write_summary(&report.summary).expect("summary written");
		let dashboard = read_dashboard(&writer.directory).expect("dashboard readable");
		assert_eq!(dashboard.plots.len(), 6);
		assert!(dashboard.summary.contains("Samples | 0"));
		let _ = fs::remove_dir_all(directory);
	}
}
