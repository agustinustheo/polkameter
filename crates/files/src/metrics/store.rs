//! Raw files -> the [`Store`] the checks read: node scrapes, the load tool's and the chain
//! recorders' series, and plugin series.
//!
//! A plugin that records its own metrics writes `plugins/<id>/series.jsonl` (the same records as
//! `chain.jsonl`) and declares them in `plugins/<id>/metrics.json`; they are merged here with
//! `job="plugin"` and `instance=<id>`, so checks read them like any other series.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

use super::{Labels, Point, Series, Store, js_number, parse_sample_line};
use crate::records::{Sample, ScrapeRecord, SeriesRecord};
use crate::registry::{self, Kind};
use crate::{FileError, RunDir};

/// Time -> (sample name, le) -> value.
type Points = BTreeMap<u64, BTreeMap<(String, Option<String>), f64>>;

/// A metric a plugin declares in `plugins/<id>/metrics.json`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PluginMetric {
	/// Family name (a counter's samples end in `_total`).
	pub name: String,
	/// Type.
	pub kind: Kind,
	/// What it measures.
	pub help: String,
	/// Upper bounds of a histogram.
	#[serde(default, skip_serializing_if = "Vec::is_empty")]
	pub buckets: Vec<f64>,
}

impl From<&registry::Def> for PluginMetric {
	fn from(def: &registry::Def) -> Self {
		Self {
			name: def.name.to_owned(),
			kind: def.kind,
			help: def.help.to_owned(),
			buckets: def.buckets.to_vec(),
		}
	}
}

/// One metric family: labels -> time -> (sample name, le) -> value.
type Family = BTreeMap<Vec<(String, String)>, Points>;

#[derive(Debug, Default)]
struct FamilySet {
	families: BTreeMap<String, Family>,
	/// Plugin metrics, by family name.
	plugin: HashMap<String, PluginMetric>,
}

impl FamilySet {
	fn def(&self, family: &str) -> Option<PluginMetric> {
		registry::def(family)
			.map(PluginMetric::from)
			.or_else(|| self.plugin.get(family).cloned())
	}

	fn add(&mut self, family: &str, sample: &str, mut labels: Labels, t: u64, value: f64) {
		if self.def(family).is_none() {
			return;
		}
		let le = labels.remove("le");
		let key: Vec<_> = labels.into_iter().collect();
		self.families
			.entry(family.to_owned())
			.or_default()
			.entry(key)
			.or_default()
			.entry(t)
			.or_default()
			.insert((sample.to_owned(), le), value);
	}

	/// Each sample name with its series, every series in time order.
	fn into_store(self) -> Store {
		let mut store = Store::new();
		for family in self.families.into_values() {
			let mut series: BTreeMap<(String, Vec<(String, String)>), Series> = BTreeMap::new();
			for (labels, points) in family {
				for (t, samples) in points {
					for ((sample, le), value) in samples {
						let mut labels: Labels = labels.iter().cloned().collect();
						if let Some(le) = le {
							labels.insert("le".into(), le);
						}
						series
							.entry((sample, labels.clone().into_iter().collect()))
							.or_insert_with(|| Series { labels, points: Vec::new() })
							.points
							.push(Point { t: t as f64, value });
					}
				}
			}
			for ((sample, _), s) in series {
				store.entry(sample).or_default().push(s);
			}
		}
		store
	}
}

/// Reads the raw files of `dir` into one [`Store`].
pub fn read_store(dir: &RunDir) -> Result<Store, FileError> {
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
	let mut series = vec![
		("chain.jsonl".to_owned(), "stress".to_owned(), "chain-recorder".to_owned()),
		("load.jsonl".to_owned(), "stress".to_owned(), "load-tool".to_owned()),
	];
	for (id, metrics) in plugin_metrics(dir)? {
		set.plugin.extend(metrics.into_iter().map(|m| (m.name.clone(), m)));
		series.push((format!("plugins/{id}/series.jsonl"), "plugin".to_owned(), id));
	}
	for (file, job, instance) in series {
		for l in dir.read_jsonl::<SeriesRecord>(&file)? {
			let mut labels = l.labels;
			labels.insert("job".into(), job.clone());
			labels.insert("instance".into(), instance.clone());
			match l.sample {
				Sample::Value { value } => set.add(&l.name, &l.name, labels, l.t, value),
				Sample::Histogram { buckets, sum } => {
					add_histogram(&mut set, &l.name, &labels, l.t, &buckets, sum)
				},
			}
		}
	}
	Ok(set.into_store())
}

/// Each plugin directory with a `metrics.json`, and the metrics it declares. A plugin may not
/// redefine a registry metric.
fn plugin_metrics(dir: &RunDir) -> Result<Vec<(String, Vec<PluginMetric>)>, FileError> {
	let root = dir.path.join("plugins");
	let Ok(entries) = std::fs::read_dir(&root) else { return Ok(Vec::new()) };
	let mut out = Vec::new();
	for entry in entries.flatten() {
		let path = entry.path().join("metrics.json");
		let Ok(text) = std::fs::read_to_string(&path) else { continue };
		let metrics: Vec<PluginMetric> = serde_json::from_str(&text).map_err(|e| {
			FileError::Io { path: path.display().to_string(), source: std::io::Error::other(e) }
		})?;
		let metrics = metrics.into_iter().filter(|m| registry::def(&m.name).is_none()).collect();
		out.push((entry.file_name().to_string_lossy().into_owned(), metrics));
	}
	out.sort_by(|a, b| a.0.cmp(&b.0));
	Ok(out)
}

fn add_histogram(
	set: &mut FamilySet,
	name: &str,
	labels: &Labels,
	t: u64,
	buckets: &[f64],
	sum: f64,
) {
	let Some(def) = set.def(name) else { return };
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
