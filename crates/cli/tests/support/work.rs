//! A temporary directory for one test, removed when dropped, even if the test panics.

use std::path::PathBuf;

pub struct Work(pub PathBuf);

impl Work {
	/// A new directory named after `name`. The path is canonical: the CLI prints canonical
	/// paths, and the temporary directory can be a symlink (macOS).
	pub fn new(name: &str) -> Self {
		let nanos = std::time::SystemTime::now()
			.duration_since(std::time::UNIX_EPOCH)
			.unwrap()
			.as_nanos();
		let directory =
			std::env::temp_dir().join(format!("polkameter-{name}-{}-{nanos}", std::process::id()));
		std::fs::create_dir_all(&directory).unwrap();
		Self(directory.canonicalize().unwrap())
	}
}

impl Drop for Work {
	fn drop(&mut self) {
		let _ = std::fs::remove_dir_all(&self.0);
	}
}
