//! The recorded smoke run in `tests/fixtures/smoke-run`.

use std::{fs, path::Path};

/// Copies the recorded run's input files (not its golden outputs) into `directory`.
pub fn copy_to(directory: &Path) {
	let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/smoke-run");
	fs::create_dir_all(directory).unwrap();
	for entry in fs::read_dir(&fixture).unwrap() {
		let path = entry.unwrap().path();
		if path.extension().is_some_and(|ext| ext == "gz") {
			let mut decoder = flate2::read::GzDecoder::new(fs::File::open(&path).unwrap());
			let mut file = fs::File::create(directory.join(path.file_stem().unwrap())).unwrap();
			std::io::copy(&mut decoder, &mut file).unwrap();
		} else if path.extension().is_some_and(|ext| ext == "json" || ext == "jsonl") {
			fs::copy(&path, directory.join(path.file_name().unwrap())).unwrap();
		}
	}
}
