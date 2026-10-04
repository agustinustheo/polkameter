//! The scenarios. Each one is a module that brings its options, sets the chain up and returns a
//! load source; the runner (`polkameter-load`) does everything else.

pub mod shared;
pub mod stmt;

pub mod setup;
pub mod tx;
