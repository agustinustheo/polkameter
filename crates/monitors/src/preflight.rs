//! Before any load: every node answers, and every metric we read has the type we expect. A
//! renamed metric or a changed type would make the data useless, so it stops the run.

use polkameter_files::registry::{Kind, NODE_METRICS};

use crate::Target;

/// An error of our tools found before the load.
#[derive(Debug, thiserror::Error)]
#[error("preflight: {0}")]
pub struct PreflightError(pub String);

/// Reads every target once. Returns warnings (metrics a node doesn't serve).
pub async fn preflight(targets: &[Target]) -> Result<Vec<String>, PreflightError> {
	let http = reqwest::Client::builder()
		.timeout(std::time::Duration::from_secs(5))
		.build()
		.expect("http client");
	let mut warnings = Vec::new();
	for t in targets {
		let body = http
			.get(&t.url)
			.send()
			.await
			.and_then(reqwest::Response::error_for_status)
			.map_err(|e| PreflightError(format!("{} at {}: {e}", t.instance, t.url)))?;
		let text = body.text().await.map_err(|e| PreflightError(format!("{}: {e}", t.instance)))?;
		for line in text.lines().filter_map(|l| l.strip_prefix("# TYPE ")) {
			let mut parts = line.split(' ');
			let (Some(name), Some(kind)) = (parts.next(), parts.next()) else { continue };
			let Some(def) = NODE_METRICS.iter().find(|d| d.name == name) else { continue };
			let want = match def.kind {
				Kind::Counter => "counter",
				Kind::Gauge => "gauge",
				Kind::Histogram => "histogram",
			};
			if kind != want {
				return Err(PreflightError(format!(
					"{}: {name} is a {kind}, the registry says {want}",
					t.instance
				)));
			}
		}
		let job = t.job.label();
		let missing: Vec<_> = NODE_METRICS
			.iter()
			.filter(|d| d.from.contains(&job) && !text.contains(d.name))
			.map(|d| d.name)
			.collect();
		if !missing.is_empty() {
			warnings.push(format!("{} ({job}) serves no {}", t.instance, missing.join(", ")));
		}
	}
	Ok(warnings)
}

/// Explicit required metric families must exist on every node of the selected role.
/// Other missing families remain warnings: some counters appear only after the first event.
pub async fn require_metrics(
	targets: &[Target],
	requirements: &[(String, String)],
) -> Result<(), PreflightError> {
	let http = reqwest::Client::builder()
		.timeout(std::time::Duration::from_secs(5))
		.build()
		.expect("http client");
	for (role, name) in requirements {
		let nodes: Vec<_> = targets.iter().filter(|target| target.job.label() == role).collect();
		if nodes.is_empty() {
			return Err(PreflightError(format!("required role {role} has no nodes")));
		}
		for node in nodes {
			let text = http
				.get(&node.url)
				.send()
				.await
				.and_then(reqwest::Response::error_for_status)
				.map_err(|e| PreflightError(e.to_string()))?
				.text()
				.await
				.map_err(|e| PreflightError(e.to_string()))?;
			let present = text.lines().any(|line| {
				line.strip_prefix("# TYPE ")
					.is_some_and(|line| line.split_whitespace().next() == Some(name.as_str()))
					|| line
						.strip_prefix(name)
						.is_some_and(|rest| rest.starts_with('{') || rest.starts_with(' '))
			});
			if !present {
				return Err(PreflightError(format!(
					"{} lacks required metric {name}",
					node.instance
				)));
			}
		}
	}
	Ok(())
}
