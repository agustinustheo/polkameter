//! The desktop app and the headless CLI over the shared XML plan engine.

pub mod cli;
#[cfg(feature = "desktop")]
pub mod desktop;
mod plugin_application;
mod remote;
