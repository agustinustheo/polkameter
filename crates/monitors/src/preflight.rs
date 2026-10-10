//! Before any load: every node answers, and every metric we read has the type we expect. A
//! renamed metric or a changed type would make the data useless, so it stops the run.

use anyhow::{Result, anyhow, bail};
use polkameter_files::registry::{Kind, NODE_METRICS};

use crate::{
	Job, Target,
	scraper::{get, http_client},
};

/// Reads every target once. Returns warnings (metrics a node doesn't serve).
pub async fn preflight(targets: &[Target]) -> Result<Vec<String>> {
	read_all(targets).await.map_err(|e| anyhow!("preflight: {e}"))
}

async fn read_all(targets: &[Target]) -> Result<Vec<String>> {
	let http = http_client();
	let mut warnings = Vec::new();
	for t in targets {
		let body = get(&http, &t.url)
			.await
			.map_err(|e| anyhow!("{} at {}: {e}", t.instance, t.url))?;
		let text = body.text().await.map_err(|e| anyhow!("{}: {e}", t.instance))?;
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
				bail!("{}: {name} is a {kind}, the registry says {want}", t.instance);
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

/// Explicit required metric families must exist on every node of their role.
/// Other missing families remain warnings: some counters appear only after the first event.
pub async fn require_metrics(targets: &[Target], requirements: &[(Job, String)]) -> Result<()> {
	check_metrics(targets, requirements)
		.await
		.map_err(|e| anyhow!("preflight: {e}"))
}

async fn check_metrics(targets: &[Target], requirements: &[(Job, String)]) -> Result<()> {
	let http = http_client();
	for (role, name) in requirements {
		let nodes: Vec<_> = targets.iter().filter(|target| target.job == *role).collect();
		if nodes.is_empty() {
			bail!("required role {} has no nodes", role.label());
		}
		for node in nodes {
			let text = get(&http, &node.url).await?.text().await?;
			let present = text.lines().any(|line| {
				line.strip_prefix("# TYPE ")
					.is_some_and(|line| line.split_whitespace().next() == Some(name.as_str()))
					|| line
						.strip_prefix(name.as_str())
						.is_some_and(|rest| rest.starts_with('{') || rest.starts_with(' '))
			});
			if !present {
				bail!("{} lacks required metric {name}", node.instance);
			}
		}
	}
	Ok(())
}
