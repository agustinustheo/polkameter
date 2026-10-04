//! JMeter-style plots of a run's `samples.jtl`: throughput, latency percentiles and failures.

use std::{collections::BTreeMap, fs, path::Path};

use serde::Deserialize;

/// One row of `samples.jtl`, as JMeter writes it.
#[derive(Clone, Debug, Deserialize)]
pub struct SampleRecord {
	#[serde(rename = "timeStamp")]
	pub timestamp: u64,
	pub elapsed: u64,
	#[serde(rename = "responseCode")]
	pub response_code: String,
	pub success: bool,
}

/// Writes the plots of `run_dir/samples.jtl` into `run_dir/plots`.
pub fn write(run_dir: &Path) -> Result<(), String> {
	let samples = csv::Reader::from_path(run_dir.join("samples.jtl"))
		.map_err(|error| error.to_string())?
		.deserialize()
		.collect::<Result<Vec<SampleRecord>, _>>()
		.map_err(|error| error.to_string())?;
	let plots = run_dir.join("plots");
	fs::create_dir_all(&plots).map_err(|error| error.to_string())?;
	for (name, svg) in [
		("throughput", throughput_svg(&samples)),
		("latency-percentiles", latency_svg(&samples)),
		("failure-breakdown", failures_svg(&samples)),
	] {
		fs::write(plots.join(format!("{name}.svg")), svg).map_err(|error| error.to_string())?;
	}
	Ok(())
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
	format!("<svg xmlns=\"http://www.w3.org/2000/svg\" width=\"960\" height=\"320\" viewBox=\"0 0 960 320\"><rect width=\"960\" height=\"320\" fill=\"#f8fafb\"/><text x=\"32\" y=\"38\" fill=\"#263744\" font-family=\"sans-serif\" font-size=\"20\" font-weight=\"700\">{title}</text><text x=\"32\" y=\"62\" fill=\"#667784\" font-family=\"sans-serif\" font-size=\"12\">{unit}</text><path d=\"M {left} 80 V {bottom} H 918\" fill=\"none\" stroke=\"#cdd8de\"/>{data}<text x=\"32\" y=\"274\" fill=\"#667784\" font-family=\"sans-serif\" font-size=\"11\">0</text><text x=\"32\" y=\"94\" fill=\"#667784\" font-family=\"sans-serif\" font-size=\"11\">{max_y:.0}</text></svg>")
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
	fn chart_explains_when_no_data_was_collected() {
		assert!(chart("Test", "units", Vec::new(), "#000").contains("No data collected"));
	}
}
