//! The headless `polkameter` CLI and its remote agent, over the shared XML plan engine.
//!
//! The `polkameter` binary calls [`cli::main`]. The desktop app reuses [`plugin_application`]
//! and [`remote`] to start, inspect and stop runs, so both front ends share one run lifecycle.
//!
//! This crate is a command-line tool, not a library: its modules are public only so the desktop
//! app and the tests in this repository can share them, and they may change in any release.
//! Plugins use `polkameter-plugin-sdk` instead.

/// The product name every user-facing message uses. Standard-error lines start with it, as in
/// `PolkaMeter: <message>` and `PolkaMeter warning: <message>`.
pub const PRODUCT_NAME: &str = "PolkaMeter";

pub mod cli;
#[doc(hidden)]
pub mod engine;
#[doc(hidden)]
pub mod init;
#[doc(hidden)]
pub mod load;
pub mod plugin_application;
pub mod remote;
pub mod shutdown;
