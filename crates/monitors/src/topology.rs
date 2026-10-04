//! The nodes a run scrapes, from zombienet's `zombie.json` (previewnet-engine writes it to its
//! data dir). zombienet picks the Prometheus ports, so they change on every restart.

use std::path::{Path, PathBuf};

use serde::Deserialize;

/// People's para id on previewnet.
pub use polkameter_files::PEOPLE_PARA_ID;

/// What a node is to us.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Job {
	/// A People collator's own node (block building, tx pool).
	PeopleCollator,
	/// The relay node inside a People collator (collation funnel).
	PeopleCollatorRelay,
	/// A relay validator (PVF, backing, disputes).
	Validator,
}

impl Job {
	/// The `job` label.
	pub fn label(self) -> &'static str {
		match self {
			Job::PeopleCollator => "people-collator",
			Job::PeopleCollatorRelay => "people-collator-relay",
			Job::Validator => "validator",
		}
	}
}

/// One node to scrape.
#[derive(Debug, Clone)]
pub struct Target {
	/// Its job.
	pub job: Job,
	/// Its name in zombie.json.
	pub instance: String,
	/// Its `/metrics` URL.
	pub url: String,
}

#[derive(Deserialize)]
struct Node {
	name: String,
	prometheus_uri: String,
	#[serde(default)]
	spec: Option<Spec>,
}

#[derive(Deserialize)]
struct Spec {
	#[serde(default)]
	full_node_prometheus_port: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct Para {
	collators: Vec<Node>,
}

#[derive(Deserialize)]
struct Relay {
	nodes: Vec<Node>,
}

#[derive(Deserialize)]
struct Zombie {
	relay: Relay,
	parachains: std::collections::HashMap<String, serde_json::Value>,
}

/// zombie.json could not be read: an error of our setup, so the run stops.
#[derive(Debug, thiserror::Error)]
#[error("{path}: {detail}")]
pub struct TopologyError {
	path: String,
	detail: String,
}

/// `ZOMBIE_JSON`, else the engine's fork or genesis network under `PPN_DIR`.
pub fn zombie_json_path() -> Option<PathBuf> {
	if let Ok(p) = std::env::var("ZOMBIE_JSON") {
		return Some(p.into());
	}
	let ppn = std::env::var("PPN_DIR").map(PathBuf::from).ok()?;
	["data-fork/zombie.json", "data/zombie.json"]
		.iter()
		.map(|p| ppn.join(p))
		.find(|p| p.exists())
}

/// Every node to scrape.
pub fn load_targets(path: &Path) -> Result<Vec<Target>, TopologyError> {
	let err = |detail: String| TopologyError { path: path.display().to_string(), detail };
	let text = std::fs::read_to_string(path).map_err(|e| err(e.to_string()))?;
	let z: Zombie = serde_json::from_str(&text).map_err(|e| err(e.to_string()))?;
	let paras = z
		.parachains
		.get(PEOPLE_PARA_ID)
		.ok_or_else(|| err(format!("no para {PEOPLE_PARA_ID}")))?;
	// One entry per para, or a list of them.
	let paras: Vec<Para> = match paras {
		serde_json::Value::Array(_) => serde_json::from_value(paras.clone()),
		other => serde_json::from_value(serde_json::Value::Array(vec![other.clone()])),
	}
	.map_err(|e| err(e.to_string()))?;
	let mut targets = Vec::new();
	for c in paras.iter().flat_map(|p| &p.collators) {
		targets.push(Target {
			job: Job::PeopleCollator,
			instance: c.name.clone(),
			url: c.prometheus_uri.clone(),
		});
		let port = c
			.spec
			.as_ref()
			.and_then(|s| s.full_node_prometheus_port.as_ref())
			.and_then(first_port);
		if let Some(port) = port {
			let url = with_port(&c.prometheus_uri, port);
			targets.push(Target {
				job: Job::PeopleCollatorRelay,
				instance: format!("{}-relay", c.name),
				url,
			});
		}
	}
	for v in &z.relay.nodes {
		targets.push(Target {
			job: Job::Validator,
			instance: v.name.clone(),
			url: v.prometheus_uri.clone(),
		});
	}
	Ok(targets)
}

fn first_port(v: &serde_json::Value) -> Option<u64> {
	v.as_u64().or_else(|| v.as_array()?.iter().find_map(serde_json::Value::as_u64))
}

fn with_port(uri: &str, port: u64) -> String {
	let (scheme, rest) = uri.split_once("://").unwrap_or(("http", uri));
	let (host_port, path) = rest.split_once('/').unwrap_or((rest, ""));
	let host = host_port.rsplit_once(':').map_or(host_port, |(h, _)| h);
	format!("{scheme}://{host}:{port}/{path}")
}
