//! Executables that tests write and then run at once, without "Text file busy" (`ETXTBSY`).
//!
//! Linux refuses to execute a file that any process has open for writing. Rust opens files
//! close-on-exec, but a test binary runs its tests on many threads: a child that another thread
//! forks while this thread has the file open keeps a copy of the descriptor until that child
//! execs, so a file written here and run at once is now and then "busy". Writing to a temporary
//! name and renaming it does not help, as the renamed file is the same file. Instead the bytes
//! are staged in a file that is never run, and `cp`, a separate process, writes the executable:
//! once `cp` has exited, no process holds the executable open for writing.
#![allow(dead_code)]

use std::path::Path;

/// Creates the executable `path` holding `contents`. `path` must not exist yet.
pub fn write_executable(path: &Path, contents: impl AsRef<[u8]>) {
	let name = path.file_name().unwrap().to_string_lossy();
	let staging = path.with_file_name(format!(".{name}.staging"));
	std::fs::write(&staging, contents).unwrap();
	copy_executable(&staging, path);
	std::fs::remove_file(&staging).unwrap();
}

/// Copies `from` to the new executable `to`, which a test may run at once.
#[cfg(unix)]
pub fn copy_executable(from: &Path, to: &Path) {
	use std::os::unix::fs::PermissionsExt as _;
	assert!(!to.exists(), "{} already exists", to.display());
	let status = std::process::Command::new("cp").arg(from).arg(to).status().unwrap();
	assert!(status.success(), "cannot copy {} to {}", from.display(), to.display());
	std::fs::set_permissions(to, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// Copies `from` to the new executable `to`. Windows has no `ETXTBSY` race to avoid.
#[cfg(not(unix))]
pub fn copy_executable(from: &Path, to: &Path) {
	assert!(!to.exists(), "{} already exists", to.display());
	std::fs::copy(from, to).unwrap();
}
