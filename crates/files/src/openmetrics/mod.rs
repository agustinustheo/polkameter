//! The build step: merges a run's raw files into one OpenMetrics file, `run.om`, and parses it
//! back for the evaluator. `promtool tsdb create-blocks-from openmetrics run.om ./data` loads it.
//!
//! OpenMetrics rules that shape the output:
//! - a family's lines are not split up: family, then series, then time;
//! - a counter's samples end in `_total`; a node counter without that suffix is written as
//!   `unknown`, so its name stays what the node and the dashboards use;
//! - histogram `le` values use the canonical rendering (Go `%g`, plus `.0`).

use std::collections::{BTreeMap, HashMap};

mod build;
mod parse;

pub use build::build_run_om;
pub use parse::parse_run_om;

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

/// One series of `run.om`: a sample name with fixed labels.
#[derive(Debug, Clone)]
pub struct Series {
	/// Every label, `job` and `instance` included.
	pub labels: Labels,
	/// In time order.
	pub points: Vec<Point>,
}

/// A parsed `run.om`: sample name -> its series.
pub type Store = HashMap<String, Vec<Series>>;

/// Go's `%g` for a float64 (shortest), with `.0` when there is no point or exponent.
pub fn canonical_number(n: f64) -> String {
	if n.is_nan() {
		return "NaN".into();
	}
	if n.is_infinite() {
		return if n > 0.0 { "+Inf" } else { "-Inf" }.into();
	}
	if n == 0.0 {
		return "0.0".into();
	}
	let exp_form = format!("{n:e}");
	let (mant, exp) = exp_form.split_once('e').expect("{:e} has an exponent");
	let exp: i32 = exp.parse().expect("exponent is a number");
	if !(-4..6).contains(&exp) {
		let sign = if exp < 0 { '-' } else { '+' };
		return format!("{mant}e{sign}{:02}", exp.abs());
	}
	let s = js_number(n);
	if s.contains(['.', 'e']) { s } else { format!("{s}.0") }
}

/// JavaScript's `String(n)`, so sample values read the same as the TS tool writes them.
pub(crate) fn js_number(n: f64) -> String {
	if n.is_nan() {
		return "NaN".into();
	}
	if n.is_infinite() {
		return if n > 0.0 { "+Inf" } else { "-Inf" }.into();
	}
	if n == 0.0 {
		return "0".into();
	}
	if n.abs() >= 1e21 || n.abs() < 1e-6 {
		let exp_form = format!("{n:e}");
		let (mant, exp) = exp_form.split_once('e').expect("{:e} has an exponent");
		return if exp.starts_with('-') {
			format!("{mant}e{exp}")
		} else {
			format!("{mant}e+{exp}")
		};
	}
	format!("{n}")
}

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
	fn canonical_numbers_follow_go() {
		assert_eq!(canonical_number(1.0), "1.0");
		assert_eq!(canonical_number(0.005), "0.005");
		assert_eq!(canonical_number(2.5), "2.5");
		assert_eq!(canonical_number(1e-5), "1e-05");
		assert_eq!(canonical_number(8_388_608.0), "8.388608e+06");
		assert_eq!(canonical_number(f64::INFINITY), "+Inf");
		assert_eq!(canonical_number(0.0), "0.0");
	}

	#[test]
	fn values_follow_js() {
		assert_eq!(js_number(5.0), "5");
		assert_eq!(js_number(0.25), "0.25");
		assert_eq!(js_number(1e-7), "1e-7");
		assert_eq!(js_number(1.5e21), "1.5e+21");
	}

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
