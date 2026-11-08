//! `results/<run id>/`: one directory per run, JSON lines appended as the run goes.

use std::{
	fs::{self, File, OpenOptions},
	io::{BufRead, BufReader, BufWriter, Write},
	path::{Path, PathBuf},
};

use serde::Serialize;

use crate::observers::files::FileError;

fn io(path: &Path) -> impl FnOnce(std::io::Error) -> FileError + '_ {
	move |source| FileError::Io { path: path.display().to_string(), source }
}

/// The directory of one run.
#[derive(Debug, Clone)]
pub struct RunDir {
	/// `<root>/<run id>`.
	pub path: PathBuf,
	/// E.g. `run-1791091539106-57209-1`.
	pub run_id: String,
}

impl RunDir {
	/// Creates `<root>/<run id>`.
	pub fn create(root: &Path, run_id: &str) -> Result<Self, FileError> {
		let path = root.join(run_id);
		fs::create_dir_all(&path).map_err(io(&path))?;
		Ok(Self { path, run_id: run_id.to_owned() })
	}

	/// An existing run directory, e.g. for `stress build`.
	pub fn open(path: &Path) -> Self {
		let run_id = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
		Self { path: path.to_owned(), run_id }
	}

	/// A raw file to append records to.
	pub fn jsonl(&self, name: &str) -> Result<JsonlWriter, FileError> {
		JsonlWriter::append(&self.path.join(name))
	}

	/// Writes `name` as pretty JSON.
	pub fn write_json(&self, name: &str, value: &impl Serialize) -> Result<(), FileError> {
		let path = self.path.join(name);
		let text = serde_json::to_string_pretty(value).map_err(|e| io(&path)(e.into()))?;
		fs::write(&path, format!("{text}\n")).map_err(io(&path))
	}

	/// Reads every record of a raw file; a missing file has none.
	pub fn read_jsonl<T: serde::de::DeserializeOwned>(
		&self,
		name: &str,
	) -> Result<Vec<T>, FileError> {
		let mut out = Vec::new();
		self.each_jsonl(name, |record| out.push(record))?;
		Ok(out)
	}

	/// Hands every record of a raw file to `f`, one line at a time, so a large file is never
	/// held whole; a missing file has none.
	pub(crate) fn each_jsonl<T: serde::de::DeserializeOwned>(
		&self,
		name: &str,
		mut f: impl FnMut(T),
	) -> Result<(), FileError> {
		let path = self.path.join(name);
		let file = match File::open(&path) {
			Ok(file) => file,
			Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
			Err(e) => return Err(io(&path)(e)),
		};
		for (i, line) in BufReader::new(file).lines().enumerate() {
			let line = line.map_err(io(&path))?;
			if line.is_empty() {
				continue;
			}
			let record = serde_json::from_str(&line).map_err(|source| FileError::Record {
				path: path.display().to_string(),
				line: i + 1,
				source,
			})?;
			f(record);
		}
		Ok(())
	}
}

/// What a [`JsonlWriter`] does to each record before it writes it.
type Filter = std::sync::Arc<dyn Fn(&mut serde_json::Value) + Send + Sync>;

/// Appends one JSON record per line. Buffered; `flush` or drop writes it out.
pub struct JsonlWriter {
	path: PathBuf,
	out: BufWriter<File>,
	filter: Option<Filter>,
}

impl std::fmt::Debug for JsonlWriter {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.debug_struct("JsonlWriter")
			.field("path", &self.path)
			.field("filtered", &self.filter.is_some())
			.finish_non_exhaustive()
	}
}

impl JsonlWriter {
	fn append(path: &Path) -> Result<Self, FileError> {
		let file = OpenOptions::new().create(true).append(true).open(path).map_err(io(path))?;
		Ok(Self { path: path.to_owned(), out: BufWriter::new(file), filter: None })
	}

	/// This writer, passing each record as JSON through `filter` before writing it, for
	/// example to redact secrets from it.
	#[must_use]
	pub fn with_filter(
		mut self,
		filter: impl Fn(&mut serde_json::Value) + Send + Sync + 'static,
	) -> Self {
		self.filter = Some(std::sync::Arc::new(filter));
		self
	}

	/// Writes one record. A failed write (a full disk, a closed pipe) or a record that cannot
	/// be serialized is an error, never a panic.
	pub fn write(&mut self, record: &impl Serialize) -> Result<(), FileError> {
		let error = |e: serde_json::Error| io(&self.path)(e.into());
		match &self.filter {
			Some(filter) => {
				let mut value = serde_json::to_value(record).map_err(error)?;
				filter(&mut value);
				serde_json::to_writer(&mut self.out, &value).map_err(error)?;
			},
			None => serde_json::to_writer(&mut self.out, record).map_err(error)?,
		}
		self.out.write_all(b"\n").map_err(io(&self.path))
	}

	/// Writes out what is buffered.
	pub fn flush(&mut self) -> Result<(), FileError> {
		self.out.flush().map_err(io(&self.path))
	}

	/// Writes out what is buffered and closes the file; a failed write is an error here, not
	/// lost in a drop.
	pub fn finish(mut self) -> Result<(), FileError> {
		self.flush()
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	fn scratch(label: &str) -> PathBuf {
		let dir =
			std::env::temp_dir().join(format!("polkameter-run-dir-{label}-{}", std::process::id()));
		fs::create_dir_all(&dir).unwrap();
		dir
	}

	#[test]
	fn a_filter_sees_every_record_before_it_is_written() {
		let dir = scratch("filter");
		let run = RunDir::open(&dir);
		let mut out = run.jsonl("filtered.jsonl").unwrap().with_filter(|value| {
			if let Some(text) = value.get_mut("text") {
				*text = serde_json::json!("hidden");
			}
		});
		out.write(&serde_json::json!({"text": "secret", "n": 1})).unwrap();
		out.finish().unwrap();
		let records: Vec<serde_json::Value> = run.read_jsonl("filtered.jsonl").unwrap();
		assert_eq!(records, vec![serde_json::json!({"text": "hidden", "n": 1})]);
		fs::remove_dir_all(&dir).unwrap();
	}
}
