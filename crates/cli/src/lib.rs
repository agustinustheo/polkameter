//! The headless `polkameter` CLI over the XML plan engine.
//!
//! The `polkameter` binary calls [`cli::main`].
//!
//! This crate is a command-line tool, not a library: its modules are public only so the tests in
//! this repository can use them, and they may change in any release. Plugins use
//! `polkameter-plugin-sdk` instead.

/// The product name every user-facing message uses. Standard-error lines start with it, as in
/// `PolkaMeter: <message>` and `PolkaMeter warning: <message>`.
pub const PRODUCT_NAME: &str = "PolkaMeter";

/// Replaces the standard `eprintln!` in this crate: it writes a line to standard error and ignores
/// a failed write. The standard macro panics when standard error is a closed pipe, as in
/// `polkameter run … 2>&1 | head -1`, which would abort a run halfway. Defined before the
/// modules, so its textual scope covers all of them.
macro_rules! eprintln {
	($($arg:tt)*) => {{
		use std::io::Write as _;
		let _ = writeln!(std::io::stderr(), $($arg)*);
	}};
}

pub mod cli;
#[doc(hidden)]
pub mod engine;
mod init;
#[doc(hidden)]
pub mod load;
pub mod redact;
mod shutdown;
