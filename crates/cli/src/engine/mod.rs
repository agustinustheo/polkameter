//! Shared XML plugin orchestration for CLI, desktop and remote execution.
pub mod execute;
pub mod plan;
pub mod plugins;

pub mod artifacts;
pub mod junit;
mod machine;
pub mod measurement;
pub mod monitors;
mod plots;
mod wiring;
mod xml;
