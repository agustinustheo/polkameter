//! `polkameter --version` and `-V` print the binary name and the Cargo package version.

use std::process::Command;

#[test]
fn version_flags_print_the_package_version() {
	let expected = format!("polkameter {}\n", env!("CARGO_PKG_VERSION"));
	for flag in ["--version", "-V"] {
		let output = Command::new(env!("CARGO_BIN_EXE_polkameter")).arg(flag).output().unwrap();
		assert!(output.status.success(), "{flag} exited with {}", output.status);
		assert_eq!(String::from_utf8(output.stdout).unwrap(), expected, "{flag}");
		assert!(output.stderr.is_empty(), "{flag} wrote to standard error");
	}
}
