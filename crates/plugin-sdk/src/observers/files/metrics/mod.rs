//! The series a run recorded, as the checks read them: [`read_store`] merges a run's raw files,
//! and [`parse_sample_line`] reads one line of a node's Prometheus `/metrics` text.

use std::collections::{BTreeMap, HashMap};

mod store;

pub use store::{PluginMetric, read_store};

/// Label name -> value.
pub type Labels = BTreeMap<String, String>;

/// One sample of a series.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Point {
	/// Milliseconds since the epoch.
	pub t: f64,
	/// The value.
	pub value: f64,
}

/// One series: a sample name with fixed labels.
#[derive(Debug, Clone)]
pub struct Series {
	/// Every label, `job` and `instance` included.
	pub labels: Labels,
	/// In time order.
	pub points: Vec<Point>,
}

/// Sample name -> its series.
pub type Store = HashMap<String, Vec<Series>>;

/// One exposition sample line: `name{a="x",b="y"} value [timestamp]`.
#[derive(Debug, Clone, PartialEq)]
pub struct SampleLine {
	/// Sample name.
	pub name: String,
	/// Labels.
	pub labels: Labels,
	/// Value.
	pub value: f64,
	/// Timestamp in seconds, if the line has one.
	pub t: Option<f64>,
}

/// Parses one exposition sample line; `None` for a line that isn't one.
pub fn parse_sample_line(line: &str) -> Option<SampleLine> {
	let b = line.as_bytes();
	let name_len = b
		.iter()
		.position(|c| !(c.is_ascii_alphanumeric() || *c == b'_' || *c == b':'))?;
	if name_len == 0 || b[0].is_ascii_digit() {
		return None;
	}
	let mut labels = Labels::new();
	let mut i = name_len;
	if b.get(i) == Some(&b'{') {
		i += 1;
		while *b.get(i)? != b'}' {
			let eq = i + line[i..].find('=')?;
			let key = line[i..eq].trim().to_owned();
			let mut value = Vec::new();
			i = eq + 2;
			while *b.get(i)? != b'"' {
				if b[i] == b'\\' {
					i += 1;
					value.push(match *b.get(i)? {
						b'n' => b'\n',
						other => other,
					});
				} else {
					value.push(b[i]);
				}
				i += 1;
			}
			labels.insert(key, String::from_utf8(value).ok()?);
			i += 1;
			if b.get(i) == Some(&b',') {
				i += 1;
			}
		}
		i += 1;
	}
	let mut rest = line[i..].split_whitespace();
	let value = parse_value(rest.next()?)?;
	let t = rest.next().and_then(|t| t.parse().ok());
	Some(SampleLine { name: line[..name_len].to_owned(), labels, value, t })
}

pub(crate) fn parse_value(s: &str) -> Option<f64> {
	match s {
		"+Inf" => Some(f64::INFINITY),
		"-Inf" => Some(f64::NEG_INFINITY),
		_ => s.parse().ok(),
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn sample_lines_parse() {
		let p = parse_sample_line(
			r#"substrate_block_height{status="best",chain="x\"y"} 1234 1790000000.123"#,
		)
		.unwrap();
		assert_eq!(p.name, "substrate_block_height");
		assert_eq!(p.labels["status"], "best");
		assert_eq!(p.labels["chain"], "x\"y");
		assert_eq!(p.value, 1234.0);
		assert_eq!(p.t, Some(1_790_000_000.123));
		assert_eq!(parse_sample_line("x_bucket{le=\"+Inf\"} +Inf").unwrap().value, f64::INFINITY);
	}
}
