//! `run.om` -> [`Store`] for the evaluator.

use std::collections::HashMap;

use super::{Point, Series, Store, parse_sample_line};

/// Parses `run.om` text.
pub fn parse_run_om(text: &str) -> Store {
	let mut index: HashMap<(String, Vec<(String, String)>), usize> = HashMap::new();
	let mut all: Vec<(String, Series)> = Vec::new();
	for line in text.lines().filter(|l| !l.is_empty() && !l.starts_with('#')) {
		let Some(p) = parse_sample_line(line) else { continue };
		let key = (p.name.clone(), p.labels.clone().into_iter().collect());
		let i = *index.entry(key).or_insert_with(|| {
			all.push((p.name.clone(), Series { labels: p.labels.clone(), points: Vec::new() }));
			all.len() - 1
		});
		all[i].1.points.push(Point { t: p.t.unwrap_or(0.0) * 1000.0, value: p.value });
	}
	let mut store = Store::new();
	for (name, series) in all {
		store.entry(name).or_default().push(series);
	}
	store
}
