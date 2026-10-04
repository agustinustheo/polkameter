//! What the monitors could not record. In stress mode these are results for the summary; in
//! smoke mode the load stops on the first one, to find tool bugs early. Shared by the monitors
//! (which record) and the runner (which reads), so it lives with the other shared records.

use std::sync::{Arc, Mutex};

/// Shared by every monitor; cheap to clone.
#[derive(Debug, Clone, Default)]
pub struct Problems(Arc<Mutex<Vec<String>>>);

impl Problems {
	/// Records one problem.
	pub fn record(&self, what: String) {
		self.0.lock().expect("no panics while holding it").push(what);
	}

	/// Every problem so far.
	pub fn all(&self) -> Vec<String> {
		self.0.lock().expect("no panics while holding it").clone()
	}
}
