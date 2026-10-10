//! The headless `polkameter` CLI and its remote agent, over the shared XML plan engine.
//!
//! The `polkameter` binary calls [`cli::main`]. The desktop app reuses [`plugin_application`]
//! and [`remote`] to start, inspect and stop runs, so both front ends share one run lifecycle.

pub mod cli;
pub mod plugin_application;
pub mod remote;
