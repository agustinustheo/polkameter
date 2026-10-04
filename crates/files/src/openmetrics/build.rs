//! Raw files -> `run.om`.

use std::collections::{BTreeMap, HashMap};

use super::{Labels, canonical_number, js_number, parse_sample_line, parse_value};
use crate::records::{Sample, ScrapeRecord, SeriesRecord};
use crate::registry::{self, Kind};
use crate::{FileError, RunDir};

/// Time -> (sample name, le) -> value.
type Points = BTreeMap<u64, BTreeMap<(String, Option<String>), f64>>;

#[derive(Debug)]
struct Family {
	kind: &'static str,
	help: &'static str,
	/// Labels without `le` -> time -> (sample name, le) -> value.
	metrics: BTreeMap<Vec<(String, String)>, Points>,
}

#[derive(Debug, Default)]
struct FamilySet {
	families: BTreeMap<String, Family>,
}

impl FamilySet {
	fn add(&mut self, family: &str, sample: &str, mut labels: Labels, t: u64, value: f64) {
		let Some(def) = registry::def(family) else { return };
		let om_name = match def.kind {
			Kind::Counter => family.strip_suffix("_total").unwrap_or(family),
			_ => family,
		};
		let kind = match def.kind {
			Kind::Counter if om_name == family => "unknown",
			Kind::Counter => "counter",
			Kind::Gauge => "gauge",
			Kind::Histogram => "histogram",
		};
		let f = self.families.entry(om_name.to_owned()).or_insert_with(|| Family {
			kind,
			help: def.help,
			metrics: BTreeMap::new(),
		});
		let le = labels.remove("le");
		let key: Vec<_> = labels.into_iter().collect();
		f.metrics
			.entry(key)
			.or_default()
			.entry(t)
			.or_default()
			.insert((sample.to_owned(), le), value);
	}

	fn render(&self) -> String {
		let mut out = String::new();
		for (name, f) in &self.families {
			out.push_str(&format!("# TYPE {name} {}\n# HELP {name} {}\n", f.kind, escape(f.help)));
			for (labels, points) in &f.metrics {
				for (t, samples) in points {
					let mut samples: Vec<_> = samples.iter().collect();
					samples.sort_by(|a, b| {
						sample_rank(a.0).partial_cmp(&sample_rank(b.0)).expect("ranks are numbers")
					});
					for ((sample, le), value) in samples {
						let mut ls: Vec<String> =
							labels.iter().map(|(k, v)| format!("{k}=\"{}\"", escape(v))).collect();
						if let Some(le) = le {
							ls.push(format!(
								"le=\"{}\"",
								canonical_number(parse_value(le).unwrap_or(f64::NAN))
							));
						}
						let ls = if ls.is_empty() {
							String::new()
						} else {
							format!("{{{}}}", ls.join(","))
						};
						out.push_str(&format!(
							"{sample}{ls} {} {:.3}\n",
							js_number(*value),
							*t as f64 / 1000.0
						));
					}
				}
			}
		}
		out.push_str("# EOF\n");
		out
	}
}

/// Within one point: buckets in `le` order, then `_count`, then `_sum`.
fn sample_rank((name, le): &(String, Option<String>)) -> (u8, f64) {
	match le {
		Some(le) => (0, parse_value(le).unwrap_or(f64::MAX)),
		None if name.ends_with("_count") => (1, 0.0),
		None if name.ends_with("_sum") => (2, 0.0),
		None => (0, 0.0),
	}
}

fn escape(s: &str) -> String {
	s.replace('\\', "\\\\").replace('"', "\\\"").replace('\n', "\\n")
}

/// Merges the raw files of `dir` into `run.om` and returns its text.
pub fn build_run_om(dir: &RunDir) -> Result<String, FileError> {
	let mut set = FamilySet::default();
	let mut failures: HashMap<(String, String), f64> = HashMap::new();
	for s in dir.read_jsonl::<ScrapeRecord>("scrapes.jsonl")? {
		let base = [("job", &s.job), ("instance", &s.instance)];
		let Some(text) = &s.text else {
			let n = failures.entry((s.job.clone(), s.instance.clone())).or_default();
			*n += 1.0;
			let labels = base.iter().map(|(k, v)| ((*k).to_owned(), (*v).clone())).collect();
			set.add(
				registry::SCRAPE_FAILED.def.name,
				registry::SCRAPE_FAILED.def.name,
				labels,
				s.t,
				*n,
			);
			continue;
		};
		for line in text.lines().filter(|l| !l.is_empty() && !l.starts_with('#')) {
			let Some(p) = parse_sample_line(line) else { continue };
			let Some(family) = registry::node_family_of(&p.name) else { continue };
			let mut labels = p.labels;
			labels.extend(base.iter().map(|(k, v)| ((*k).to_owned(), (*v).clone())));
			set.add(family.name, &p.name, labels, s.t, p.value);
		}
	}
	for (file, instance) in [("chain.jsonl", "chain-recorder"), ("load.jsonl", "load-tool")] {
		for l in dir.read_jsonl::<SeriesRecord>(file)? {
			let mut labels = l.labels;
			labels.insert("job".into(), "stress".into());
			labels.insert("instance".into(), instance.into());
			match l.sample {
				Sample::Value { value } => set.add(&l.name, &l.name, labels, l.t, value),
				Sample::Histogram { buckets, sum } => {
					add_histogram(&mut set, &l.name, &labels, l.t, &buckets, sum)
				},
			}
		}
	}
	let text = set.render();
	let path = dir.path.join("run.om");
	std::fs::write(&path, &text)
		.map_err(|source| FileError::Io { path: path.display().to_string(), source })?;
	Ok(text)
}

fn add_histogram(
	set: &mut FamilySet,
	name: &str,
	labels: &Labels,
	t: u64,
	buckets: &[f64],
	sum: f64,
) {
	let Some(def) = registry::def(name) else { return };
	let bounds = def.buckets.iter().copied().chain([f64::INFINITY]);
	for (bound, count) in bounds.zip(buckets) {
		let mut l = labels.clone();
		l.insert("le".into(), if bound.is_infinite() { "+Inf".into() } else { js_number(bound) });
		set.add(name, &format!("{name}_bucket"), l, t, *count);
	}
	set.add(
		name,
		&format!("{name}_count"),
		labels.clone(),
		t,
		buckets.last().copied().unwrap_or(0.0),
	);
	set.add(name, &format!("{name}_sum"), labels.clone(), t, sum);
}
