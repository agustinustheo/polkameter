//! Writes our own `polkameter_*` series as JSON lines ([`SeriesRecord`]).
//!
//! - gauge: written at once;
//! - counter: the running total;
//! - histogram: cumulative buckets in registry order, then `+Inf`.
//!
//! Counters and histograms change per tx, so they are written at most once a second, with the
//! time of their last change. A gauge first writes what is pending, so a step or phase change
//! never moves an earlier change past it. The owner calls [`SeriesWriter::tick`]; there is no
//! timer inside, so the writer has one owner and no lock.

use std::collections::BTreeMap;

use crate::{
	FileError, JsonlWriter,
	records::{Millis, Sample, SeriesRecord},
	registry::{Def, Metric, kind},
};

const FLUSH_EVERY_MS: Millis = 1_000;

#[derive(Debug)]
struct Pending {
	record: SeriesRecord,
	dirty: bool,
}

/// One raw series file with its pending counters and histograms.
#[derive(Debug)]
pub struct SeriesWriter {
	out: JsonlWriter,
	series: BTreeMap<(String, Vec<String>), Pending>,
	last_flush: Millis,
}

fn labels(names: &[&str], values: &[String]) -> BTreeMap<String, String> {
	names.iter().zip(values).map(|(k, v)| ((*k).to_owned(), v.clone())).collect()
}

fn owned<const N: usize>(values: [&str; N]) -> Vec<String> {
	values.iter().map(|s| s.to_string()).collect()
}

impl SeriesWriter {
	/// Series go to `out`.
	pub fn new(out: JsonlWriter) -> Self {
		Self { out, series: BTreeMap::new(), last_flush: 0 }
	}

	/// Writes a gauge now, after whatever is pending.
	pub fn gauge<const N: usize>(
		&mut self,
		m: &Metric<kind::Gauge, N>,
		values: [&str; N],
		value: f64,
		t: Millis,
	) -> Result<(), FileError> {
		self.flush()?;
		self.out.write(&SeriesRecord {
			t,
			name: m.def.name.to_owned(),
			labels: labels(m.def.labels, &owned(values)),
			sample: Sample::Value { value },
		})
	}

	/// Adds `by` to a counter. Nothing is written until the next flush.
	pub fn inc<const N: usize>(
		&mut self,
		m: &Metric<kind::Counter, N>,
		values: [&str; N],
		by: f64,
		t: Millis,
	) {
		let p = self.pending(&m.def, &owned(values), t, || Sample::Value { value: 0.0 });
		if let Sample::Value { value } = &mut p.record.sample {
			*value += by;
		}
	}

	/// Observes one value of a histogram. Nothing is written until the next flush.
	pub fn observe<const N: usize>(
		&mut self,
		m: &Metric<kind::Histogram, N>,
		values: [&str; N],
		v: f64,
		t: Millis,
	) {
		let bounds = m.def.buckets;
		let p = self.pending(&m.def, &owned(values), t, || Sample::Histogram {
			buckets: vec![0.0; bounds.len() + 1],
			sum: 0.0,
		});
		if let Sample::Histogram { buckets, sum } = &mut p.record.sample {
			for (count, bound) in buckets.iter_mut().zip(bounds.iter().chain([&f64::INFINITY])) {
				if v <= *bound {
					*count += 1.0;
				}
			}
			*sum += v;
		}
	}

	/// Writes pending series if the last write is a second old.
	pub fn tick(&mut self, now: Millis) -> Result<(), FileError> {
		if now.saturating_sub(self.last_flush) < FLUSH_EVERY_MS {
			return Ok(());
		}
		self.last_flush = now;
		self.flush()
	}

	/// Writes every series that changed since the last write.
	pub fn flush(&mut self) -> Result<(), FileError> {
		for p in self.series.values_mut().filter(|p| p.dirty) {
			p.dirty = false;
			self.out.write(&p.record)?;
		}
		self.out.flush()
	}

	fn pending(
		&mut self,
		def: &Def,
		values: &[String],
		t: Millis,
		empty: impl FnOnce() -> Sample,
	) -> &mut Pending {
		let key = (def.name.to_owned(), values.to_vec());
		let p = self.series.entry(key).or_insert_with(|| Pending {
			record: SeriesRecord {
				t,
				name: def.name.to_owned(),
				labels: labels(def.labels, values),
				sample: empty(),
			},
			dirty: true,
		});
		p.record.t = t;
		p.dirty = true;
		p
	}
}
