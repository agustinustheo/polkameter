//! Raw files -> the [`Store`] the checks read: node scrapes, the load tool's and the chain
//! recorders' series, and plugin series.
//!
//! A plugin that records its own metrics writes `plugins/<id>/series.jsonl` (the same records as
//! `chain.jsonl`) and declares them in `plugins/<id>/metrics.json`; they are merged here with
//! `job="plugin"` and `instance=<id>`, so checks read them like any other series.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

use super::{Labels, Point, Series, Store, parse_sample_line};
use crate::{
	FileError, RunDir, num,
	records::{Sample, ScrapeRecord, SeriesRecord},
	registry::{self, Kind},
};

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

/// A plugin's declaration of one of its metrics, from the registry definition it writes.
/// Part of the plugin API (used by out-of-tree plugins).
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

/// The samples read so far: (sample name, labels with `le`) -> time -> value.
#[derive(Debug, Default)]
struct Samples {
	samples: BTreeMap<(String, Labels), BTreeMap<u64, f64>>,
	/// Plugin metrics, by family name.
	plugin: HashMap<String, PluginMetric>,
}

impl Samples {
	/// The histogram bounds of a family, from the registry or a plugin; `None` when neither
	/// defines it.
	fn buckets(&self, family: &str) -> Option<&[f64]> {
		match registry::def(family) {
			Some(def) => Some(def.buckets),
			None => self.plugin.get(family).map(|m| m.buckets.as_slice()),
		}
	}

	fn add(&mut self, family: &str, sample: &str, labels: Labels, t: u64, value: f64) {
		if self.buckets(family).is_some() {
			self.samples.entry((sample.to_owned(), labels)).or_default().insert(t, value);
		}
	}

	fn into_store(self) -> Store {
		let mut store = Store::new();
		for ((sample, labels), points) in self.samples {
			let points =
				points.into_iter().map(|(t, value)| Point { t: t as f64, value }).collect();
			store.entry(sample).or_default().push(Series { labels, points });
		}
		store
	}
}

/// Reads the raw files of `dir` into one [`Store`].
pub fn read_store(dir: &RunDir) -> Result<Store, FileError> {
	let mut set = Samples::default();
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
	set: &mut Samples,
	name: &str,
	labels: &Labels,
	t: u64,
	buckets: &[f64],
	sum: f64,
) {
	let Some(bounds) = set.buckets(name).map(<[f64]>::to_vec) else { return };
	let bounds = bounds.iter().copied().chain([f64::INFINITY]);
	for (bound, count) in bounds.zip(buckets) {
		let mut l = labels.clone();
		l.insert("le".into(), if bound.is_infinite() { "+Inf".into() } else { num(bound) });
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
