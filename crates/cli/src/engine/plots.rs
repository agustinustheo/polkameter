//! JMeter-style plots of a run's `samples.jtl`: throughput, latency percentiles and failures.
//!
//! The file is read row by row into bounded aggregates (counts per second, per latency value
//! and per failure code), never held whole, so a long run's samples fit in little memory.
use anyhow::Result;
use polkameter_plugin_sdk::observers::files::Millis;
use serde::Deserialize;
use std::{collections::BTreeMap, fs, path::Path};

/// One row of `samples.jtl`, as JMeter writes it.
#[derive(Deserialize)]
struct SampleRow {
	#[serde(rename = "timeStamp")]
	timestamp: u64,
	elapsed: Millis,
	#[serde(rename = "responseCode")]
	response_code: String,
	success: bool,
}

/// The rows of `samples.jtl`, read one at a time.
fn rows(path: &Path) -> Result<impl Iterator<Item = Result<SampleRow>>> {
	let reader = csv::Reader::from_path(path)?;
	Ok(reader.into_deserialize().map(|row| row.map_err(Into::into)))
}

/// What the plots need from the samples.
#[derive(Default)]
struct Aggregates {
	/// The earliest timestamp.
	first: Option<u64>,
	/// Samples per latency, in ms.
	latencies: BTreeMap<Millis, u64>,
	/// Failed samples per response code.
	failures: BTreeMap<String, u64>,
	/// Samples per second since `first` (filled in a second pass).
	per_second: BTreeMap<u64, u64>,
}

/// Writes the plots of `run_dir/samples.jtl` into `run_dir/plots`.
pub fn write(run_dir: &Path) -> Result<()> {
	let path = run_dir.join("samples.jtl");
	let mut a = Aggregates::default();
	for row in rows(&path)? {
		let row = row?;
		a.first = Some(a.first.map_or(row.timestamp, |f| f.min(row.timestamp)));
		*a.latencies.entry(row.elapsed).or_default() += 1;
		if !row.success {
			*a.failures.entry(row.response_code).or_default() += 1;
		}
	}
	// Seconds count from the earliest sample, which only the first pass knows.
	let first = a.first.unwrap_or_default();
	for row in rows(&path)? {
		*a.per_second.entry(row?.timestamp.saturating_sub(first) / 1_000).or_default() += 1;
	}
	let plots = run_dir.join("plots");
	fs::create_dir_all(&plots)?;
	for (name, svg) in [
		("throughput", throughput_svg(&a)),
		("latency-percentiles", latency_svg(&a)),
		("failure-breakdown", failures_svg(&a)),
	] {
		fs::write(plots.join(format!("{name}.svg")), svg)?;
	}
	Ok(())
}

fn throughput_svg(a: &Aggregates) -> String {
	let points = a.per_second.iter().map(|(s, n)| (*s, *n)).collect();
	chart("Throughput", "samples / second", points, "#2d9d8b")
}

/// The value at percentile `p` of the counted values, as `load::steps::percentile` picks it
/// from the sorted list: index `floor(p / 100 * n)`, the last one at most.
fn percentile(counts: &BTreeMap<Millis, u64>, p: f64) -> Millis {
	let n: u64 = counts.values().sum();
	if n == 0 {
		return 0;
	}
	let i = (((p / 100.0) * n as f64).floor() as u64).min(n - 1);
	let mut seen = 0;
	for (value, count) in counts {
		seen += count;
		if i < seen {
			return *value;
		}
	}
	0
}

fn latency_svg(a: &Aggregates) -> String {
	chart(
		"Latency percentiles",
		"milliseconds",
		vec![
			(50, percentile(&a.latencies, 50.0)),
			(95, percentile(&a.latencies, 95.0)),
			(99, percentile(&a.latencies, 99.0)),
		],
		"#486fb5",
	)
}

fn failures_svg(a: &Aggregates) -> String {
	let points = a
		.failures
		.values()
		.enumerate()
		.map(|(index, value)| (index as u64 + 1, *value))
		.collect();
	chart("Failure breakdown", "failed samples", points, "#c95c4d")
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
	let data = if points.is_empty() {
		"<text x=\"488\" y=\"180\" text-anchor=\"middle\" fill=\"#667784\" font-family=\"sans-serif\" font-size=\"14\">No data collected</text>".into()
	} else {
		format!("<path d=\"{path}\" fill=\"none\" stroke=\"{color}\" stroke-width=\"3\"/>")
	};
	format!(
		"<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"960\" height=\"320\" viewBox=\"0 0 960 320\"><rect width=\"960\" height=\"320\" fill=\"#f8fafb\"/><text x=\"32\" y=\"38\" fill=\"#263744\" font-family=\"sans-serif\" font-size=\"20\" font-weight=\"700\">{title}</text><text x=\"32\" y=\"62\" fill=\"#667784\" font-family=\"sans-serif\" font-size=\"12\">{unit}</text><path d=\"M {left} 80 V {bottom} H 918\" fill=\"none\" stroke=\"#cdd8de\"/>{data}<text x=\"32\" y=\"274\" fill=\"#667784\" font-family=\"sans-serif\" font-size=\"11\">0</text><text x=\"32\" y=\"94\" fill=\"#667784\" font-family=\"sans-serif\" font-size=\"11\">{max_y:.0}</text></svg>"
	)
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn counted_percentiles_match_the_sorted_list() {
		let values: Vec<Millis> = (0..997u64).map(|i| (i * 7919) % 613).collect();
		let mut counts = BTreeMap::new();
		for v in &values {
			*counts.entry(*v).or_default() += 1;
		}
		for p in [0.0, 50.0, 95.0, 99.0, 100.0] {
			assert_eq!(percentile(&counts, p), crate::load::steps::percentile(&values, p), "{p}");
		}
		assert_eq!(percentile(&BTreeMap::new(), 50.0), 0);
	}

	#[test]
	fn chart_explains_when_no_data_was_collected() {
		assert!(chart("Test", "units", Vec::new(), "#000").contains("No data collected"));
	}
}
