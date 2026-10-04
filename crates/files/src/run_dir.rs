//! `results/<run id>/`: one directory per run, JSON lines appended as the run goes.

use std::fs::{self, File, OpenOptions};
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::FileError;

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
		let text = serde_json::to_string_pretty(value).expect("our records serialize");
		fs::write(&path, format!("{text}\n")).map_err(io(&path))
	}

	/// Reads every record of a raw file; a missing file has none.
	pub fn read_jsonl<T: serde::de::DeserializeOwned>(
		&self,
		name: &str,
	) -> Result<Vec<T>, FileError> {
		let path = self.path.join(name);
		let text = match fs::read_to_string(&path) {
			Ok(text) => text,
			Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
			Err(e) => return Err(io(&path)(e)),
		};
		text.lines()
			.enumerate()
			.filter(|(_, l)| !l.is_empty())
			.map(|(i, l)| {
				serde_json::from_str(l).map_err(|source| FileError::Record {
					path: path.display().to_string(),
					line: i + 1,
					source,
				})
			})
			.collect()
	}
}

/// Appends one JSON record per line. Buffered; `flush` or drop writes it out.
#[derive(Debug)]
pub struct JsonlWriter {
	path: PathBuf,
	out: BufWriter<File>,
}

impl JsonlWriter {
	fn append(path: &Path) -> Result<Self, FileError> {
		let file = OpenOptions::new().create(true).append(true).open(path).map_err(io(path))?;
		Ok(Self { path: path.to_owned(), out: BufWriter::new(file) })
	}

	/// Writes one record.
	pub fn write(&mut self, record: &impl Serialize) -> Result<(), FileError> {
		serde_json::to_writer(&mut self.out, record).expect("our records serialize");
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
