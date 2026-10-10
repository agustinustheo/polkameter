//! JMeter-style plots of a run's `samples.jtl`: throughput, latency percentiles and failures.
use anyhow::Result;
use polkameter_files::Millis;
use polkameter_load::steps::percentile;
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

/// Writes the plots of `run_dir/samples.jtl` into `run_dir/plots`.
pub fn write(run_dir: &Path) -> Result<()> {
	let samples = csv::Reader::from_path(run_dir.join("samples.jtl"))?
		.deserialize()
		.collect::<Result<Vec<SampleRow>, _>>()?;
	let plots = run_dir.join("plots");
	fs::create_dir_all(&plots)?;
	for (name, svg) in [
		("throughput", throughput_svg(&samples)),
		("latency-percentiles", latency_svg(&samples)),
		("failure-breakdown", failures_svg(&samples)),
	] {
		fs::write(plots.join(format!("{name}.svg")), svg)?;
	}
	Ok(())
}

fn throughput_svg(samples: &[SampleRow]) -> String {
	let first = samples.iter().map(|sample| sample.timestamp).min().unwrap_or_default();
	let mut buckets = BTreeMap::<u64, u64>::new();
	for sample in samples {
		*buckets.entry(sample.timestamp.saturating_sub(first) / 1_000).or_default() += 1;
	}
	chart("Throughput", "samples / second", buckets.into_iter().collect(), "#2d9d8b")
}

fn latency_svg(samples: &[SampleRow]) -> String {
	let values: Vec<Millis> = samples.iter().map(|sample| sample.elapsed).collect();
	chart(
		"Latency percentiles",
		"milliseconds",
		vec![
			(50, percentile(&values, 50.0)),
			(95, percentile(&values, 95.0)),
			(99, percentile(&values, 99.0)),
		],
		"#486fb5",
	)
}

fn failures_svg(samples: &[SampleRow]) -> String {
	let mut counts = BTreeMap::<&str, u64>::new();
	for sample in samples.iter().filter(|sample| !sample.success) {
		*counts.entry(&sample.response_code).or_default() += 1;
	}
	let points = counts
		.into_values()
		.enumerate()
		.map(|(index, value)| (index as u64 + 1, value))
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
	fn chart_explains_when_no_data_was_collected() {
		assert!(chart("Test", "units", Vec::new(), "#000").contains("No data collected"));
	}
}
