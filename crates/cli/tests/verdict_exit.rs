//! The verdict a monitorless run's report reaches.
use serde_json::{Value, json};
#[path = "support/work.rs"]
mod work;

use std::{
	fs,
	path::{Path, PathBuf},
};
use work::Work;

fn repository() -> PathBuf {
	Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// A run without `<monitors>`, rebuilt by `polkameter report`, is not INCONCLUSIVE: the checks
/// of nodes it does not monitor are not applicable and optional.
#[test]
fn a_monitorless_run_is_not_inconclusive() {
	let work = Work::new("monitorless");
	// The recorded run's own files, less what only monitors write (node scrapes in scrapes.jsonl,
	// the relay chain in chain.jsonl) and the plugin's verdicts: what a run of the same load
	// without <monitors> leaves behind.
	let fixture = repository().join("crates/cli/tests/fixtures/smoke-run");
	for name in
		["events.jsonl", "execution.json", "load.jsonl", "steps.jsonl", "transactions.jsonl"]
	{
		fs::copy(fixture.join(name), work.0.join(name)).unwrap();
	}
	let mut summary: Value =
		serde_json::from_slice(&fs::read(fixture.join("summary.json")).unwrap()).unwrap();
	// Without <monitors>, the run records no parachain and no relay chain, and the latency
	// breaking point of the recorded run (a separate FAIL) is left out.
	summary["network"]["paraId"] = Value::Null;
	summary["breakingPoint"] = Value::Null;
	summary["mode"] = json!("stress");
	for key in ["checks", "verdict"] {
		summary.as_object_mut().unwrap().remove(key);
	}
	fs::write(work.0.join("summary.json"), summary.to_string()).unwrap();

	let output = std::process::Command::new(env!("CARGO_BIN_EXE_polkameter"))
		.arg("report")
		.arg(&work.0)
		.env("POLKAMETER_PLUGIN_REGISTRY", work.0.join("plugins.json"))
		.output()
		.unwrap();
	assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
	let summary: Value =
		serde_json::from_slice(&fs::read(work.0.join("summary.json")).unwrap()).unwrap();
	let checks = summary["checks"].as_array().unwrap();
	let not_applicable: Vec<&Value> = checks
		.iter()
		.filter(|c| c["detail"].as_str().unwrap().starts_with("not applicable:"))
		.collect();
	assert_eq!(not_applicable.len(), 7, "{checks:#?}");
	for check in &not_applicable {
		assert_eq!(check["status"], "no result", "{check}");
		assert_eq!(check["optional"], true, "{check}");
	}
	let required_gaps: Vec<&Value> = checks
		.iter()
		.filter(|c| c["status"] == "no result" && c["optional"] != true)
		.collect();
	assert!(required_gaps.is_empty(), "{required_gaps:#?}");
	let verdict = summary["verdict"]["result"].as_str().unwrap();
	assert!(matches!(verdict, "PASS" | "WARN"), "verdict {verdict}: {checks:#?}");
	let junit = fs::read_to_string(work.0.join("junit.xml")).unwrap();
	assert!(!junit.contains("INCONCLUSIVE"), "{junit}");
}
