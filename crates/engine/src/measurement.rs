//! The measured load: baseline, rate steps, recovery and reconciliation of prepared transactions,
//! with the node monitors. It returns the summary; the caller writes the report once plugin checks
//! have run.
use crate::{
	execute::{Values, resolve, transactions},
	machine,
	plan::Plan,
	plugins::{Plugins, Registry},
	wiring::{Chains, Monitors},
};
use anyhow::{Context as _, Result, bail};
use polkameter_chain::{ChainError, Client};
use polkameter_files::summary::{
	Artifact, Loss, LostTx, MaxSustained, Network, NodePool, SCHEMA_VERSION, Summary,
};
use polkameter_files::{FinalStep, NodeMax, NodeSample, RunDir};
use polkameter_load::runner::{Io, Mode, RunOptions, baseline, drain, load as apply_load, recover};
use polkameter_load::sender::Sender;
use polkameter_load::steps::final_step;
use polkameter_load::tracker::{Phase, Tracker};
use polkameter_load::{Lane, QueueSource, StateCheck, Tx, TxHash, follower, now_ms, rules};
use polkameter_plugin_sdk::Context;
use std::path::Path;
use tokio_util::sync::CancellationToken;

/// A finished measurement, before the report.
pub struct Measured {
	summary: Summary,
	finals: Vec<FinalStep>,
	mode: Mode,
}

/// File of the plugin check results, kept so `polkameter report` can write the same summary.
const PLUGIN_CHECKS: &str = "plugin-checks.json";

pub async fn run(
	plan: &Plan,
	plugins: &Plugins,
	registry: &Registry,
	values: &Values,
	context: &Context,
	setup_seconds: u64,
	progress: &(dyn Fn(&str) -> Result<()> + Send + Sync),
) -> Result<Measured> {
	let dir = &RunDir::open(&context.artifact_dir);
	let load = plan.load.as_ref().context("load missing")?;
	let configured_rules = plan.thresholds.rules()?;
	let mode: Mode = plan.mode.parse().map_err(anyhow::Error::msg)?;
	let url = values[&format!("targets.{}", load.target)]
		.as_str()
		.context("target missing")?
		.to_owned();
	let client = Client::connect(&url).await?;
	let chain = client.chain_info().await?;
	let block_interval_s = load.block_interval_seconds;
	let flood = transactions(&resolve(values, &load.source)?, &context.artifact_dir)?;
	let probes = transactions(&resolve(values, &load.probes_ref)?, &context.artifact_dir)?;
	anyhow::ensure!(
		probes.len()
			>= polkameter_load::probe_count(
				load.baseline_probes,
				load.recovery_seconds,
				block_interval_s
			),
		"insufficient probe reserve"
	);
	let lanes = vec![Lane {
		call: "prepared",
		source: Box::new(QueueSource::new(
			flood.iter().map(|t| Ok(Tx::new(t.decode()?))).collect::<Result<Vec<_>>>()?,
			probes.iter().map(|t| Ok(Tx::new(t.decode()?))).collect::<Result<Vec<_>>>()?,
			"prepared transactions",
		)),
	}];
	let mut seen = std::collections::HashSet::new();
	for tx in flood.iter().chain(&probes) {
		anyhow::ensure!(seen.insert(&tx.hash), "duplicate transaction in flood/probe sources");
	}
	let state_check = load
		.state_check
		.as_ref()
		.map(|operation| -> Result<_> {
			Ok(PluginState {
				plugins: plugins.clone(),
				operation: operation.clone(),
				target: url.clone(),
				state: resolve(values, load.state_ref.as_ref().unwrap())?,
				context: context.clone(),
			})
		})
		.transpose()?;
	let (targets, relay_url) = if let Some(monitors) = &plan.monitors {
		let topology = registry
			.topologies
			.get(&monitors.topology)
			.context("topology alias not installed on this host")?;
		let targets = polkameter_monitors::load_targets(topology, monitors.para_id)?;

		(
			targets,
			values[&format!("targets.{}", monitors.relay_target)]
				.as_str()
				.context("relay target missing")?
				.to_owned(),
		)
	} else {
		(vec![], url.clone())
	};
	let run_opts = RunOptions {
		rules: configured_rules,
		mode,
		plan: load.rate_plan()?,
		connections: load.connections,
		recovery_s: load.recovery_seconds,
		baseline_probes: load.baseline_probes,
		block_interval_s,
	};
	// Monitors and the load.
	let (events, _) = tokio::sync::broadcast::channel(64);
	let chains =
		Chains { node_url: &url, relay_url: plan.monitors.as_ref().map(|_| relay_url.as_str()) };
	let mut monitors = Monitors::start(dir, targets, &events, chains).await?;
	let (replies_tx, replies) = tokio::sync::mpsc::unbounded_channel();
	let (blocks_tx, blocks) = tokio::sync::mpsc::unbounded_channel();
	let sender = Sender::open(&url, load.connections, replies_tx).await?;
	let follow = CancellationToken::new();
	let _follow_guard = follow.clone().drop_guard();
	follower::start(client.clone(), client.normal_limit().await?, blocks_tx, follow.clone());
	let mut tracker = Tracker::new(
		lanes,
		sender,
		dir.jsonl("load.jsonl")?,
		dir.jsonl("blocks.jsonl")?,
		load.baseline_probes,
		now_ms(),
	);
	let mut io = Io {
		replies,
		blocks,
		events,
		problems: monitors.problems.clone(),
		tool_failed: monitors.tool_failed.clone(),
	};

	let phases = async {
		progress("baseline")?;
		let base = baseline(&mut tracker, &mut io, &run_opts).await?;
		let threshold_ms = (2 * base.max_ms).max((2000.0 * block_interval_s) as u64);
		progress("load")?;
		let stop = apply_load(&mut tracker, &mut io, &run_opts).await?;
		eprintln!("stop: {}: {}", stop.rule.name(), stop.detail);
		progress("recovery")?;
		let recovery_from = now_ms();
		let recovery = recover(&mut tracker, &mut io, &run_opts, threshold_ms).await?;
		let recovery_to = now_ms();
		// Every block the follower still has queued, so no inclusion is missed by the loss check.
		if !drain(&mut tracker, &mut io, 30_000).await? {
			eprintln!(
				"drain: block {} not read after 30 s (last read {})",
				tracker.last_head.1, tracker.last_fetched
			);
		}
		Ok::<_, anyhow::Error>((base, stop, recovery, (recovery_from, recovery_to)))
	};
	let (base, stop, mut recovery, recovery_window) = match phases.await {
		Ok(r) => r,
		Err(e) => {
			follow.cancel();
			// A monitor's own error says more than "a monitor failed".
			if let Err(m) = monitors.stop().await {
				bail!("{e}: {m}");
			}
			bail!(e);
		},
	};
	follow.cancel();
	tracker.set_phase(Phase::Done, now_ms())?;
	// The node's CPU and memory per step come from node.jsonl, complete once the sampler stopped.
	monitors.stop_sampling().await?;
	let samples: Vec<NodeSample> = dir.read_jsonl("node.jsonl")?;
	recovery.node = NodeMax::over(&samples, recovery_window.0, recovery_window.1);
	let finals = write_steps(dir, &tracker, &["prepared"], &samples)?;
	progress("reconciliation")?;
	let (loss, lost) = loss_check(
		&client,
		&monitors,
		&mut tracker,
		&finals,
		dir,
		configured_rules.finality_wait_ms,
		state_check.as_ref().map(|s| s as &dyn polkameter_load::StateCheck),
	)
	.await;
	let mut lost_out = dir.jsonl("lost.jsonl")?;
	for l in &lost {
		lost_out.write(l)?;
	}
	lost_out.flush()?;
	let unreadable = tracker.unreadable.clone();
	tracker.finish()?.close();
	monitors.stop_chain().await?;
	let mut problems = monitors.problems.all();
	problems.extend(unreadable.into_iter().map(|u| format!("blocks: {u}")));
	monitors.stop().await?;

	// Summary.
	let bp = rules::breaking_point_with_rules(&finals, &configured_rules);
	let sustained = match &bp {
		None => finals.last(),
		Some(b) => finals.iter().find(|f| f.step + 1 == b.step),
	};
	let summary = Summary {
		schema_version: SCHEMA_VERSION,
		scenario: plan.name.clone(),
		artifact: Some(Artifact {
			name: "prepared extrinsics".into(),
			description:
				"Each operation is prepared by an installed plugin and submitted by Polkameter"
					.into(),
			context: "XML plugin workload".into(),
		}),
		run_id: dir.run_id.clone(),
		mode: mode.to_string(),
		params: serde_json::json!({ "plan": plan, "connections": load.connections }),
		budget: format!("{} flood transactions and {} reserved probes", flood.len(), probes.len()),
		setup_seconds,
		extra: serde_json::json!({"genesis":hex::encode(chain.genesis),"transactionVersion":chain.tx_version,"nodeVersion":client.request::<String>("system_version",subxt_rpcs::rpc_params![]).await.ok()}),
		rules: serde_json::to_value(configured_rules)?,
		failure_modes: rules::failure_modes(&stop, &loss, &finals),
		stop,
		breaking_point: bp,
		max_sustained: sustained.map(|f| MaxSustained {
			step: f.step,
			target_rate: f.target_rate,
			included_per_s: f.included_per_s,
		}),
		recovery,
		loss,
		baseline: base,
		problems,
		network: Network {
			url,
			para_id: plan.monitors.as_ref().map(|m| m.para_id),
			spec_version: chain.spec_version,
			block_interval_s: (block_interval_s * 100.0).round() / 100.0,
		},
		runner: machine::runner(),
		steps: run_opts.plan.steps.len(),
		checks: None,
	};
	// Plugin checks read it, with the raw files, before the report adds the checks.
	dir.write_json("summary.json", &summary)?;
	Ok(Measured { summary, finals, mode })
}

/// Writes run.om, the checks (the plugins' after the built-in ones), summary.json and summary.md,
/// and returns the exit code.
pub fn report(
	dir: &RunDir,
	measured: Measured,
	plugin_checks: &[polkameter_checks::CheckResult],
) -> Result<i32> {
	dir.write_json(PLUGIN_CHECKS, &plugin_checks)?;
	let Measured { summary, finals, mode } = measured;
	let checks = polkameter_checks::report::write(dir, summary.clone(), &finals, plugin_checks)?;
	Ok(exit_code(mode, &summary, &checks))
}

/// The loss check, with the node's pool counts read from the collator we submit to after the
/// finality wait (the loss check awaits the read then).
async fn loss_check<S: polkameter_load::submit::Submit>(
	client: &Client,
	monitors: &Monitors,
	tracker: &mut Tracker<S>,
	finals: &[FinalStep],
	dir: &RunDir,
	finality_wait_ms: u64,
	state: Option<&dyn polkameter_load::StateCheck>,
) -> (Loss, Vec<LostTx>) {
	let outstanding = tracker.outstanding();
	let included_ok: Vec<_> = tracker.included_ok.concat();
	let scraper = monitors.scraper.clone();
	let node_pool = async move {
		scraper.scrape_now().await;
		let s = scraper.collator.borrow().clone()?;
		Some(NodePool {
			mempool: s.sum("substrate_sub_txpool_unwatched_txs")? as u64,
			ready: s.sum("substrate_ready_transactions_number")? as u64,
		})
	};
	let settled = polkameter_load::loss::Settled {
		finals,
		outstanding: &outstanding,
		included_ok: &included_ok,
		last_ours_block: tracker.last_ours_block.max(tracker.last_fetched),
		flood: &tracker.flood,
		included_in: &tracker.included_in,
		first_fetched: tracker.first_fetched,
	};
	let (mut loss, lost, records) =
		polkameter_load::loss::loss_check(client, settled, node_pool, state, finality_wait_ms)
			.await;
	if let Err(error) = (|| -> Result<()> {
		let mut out = dir.jsonl("transactions.jsonl")?;
		for record in &records {
			out.write(record)?;
		}
		out.finish()?;
		Ok(())
	})() {
		loss.note = Some(format!(
			"{}; transaction ledger could not be written: {error}",
			loss.note.unwrap_or_default()
		));
	}
	eprintln!(
		"loss check: {} sent, {} included, {} refused, {:?} in the pool, {:?} lost",
		loss.sent, loss.included, loss.refused, loss.in_pool, loss.lost
	);
	let c = &loss.on_chain;
	eprintln!(
		"loss check on the finalized chain (blocks {}-{}): {} included, {} the tracker missed, {} only on a fork, {} moved",
		c.blocks.0, c.blocks.1, c.included, c.missed, c.only_on_fork, c.moved
	);
	(loss, lost)
}

/// steps.jsonl: one record per step and lane (no lane name in a single-lane run, as TS), with
/// the node's highest CPU and memory while the step ran.
fn write_steps<S: polkameter_load::submit::Submit>(
	dir: &RunDir,
	tracker: &Tracker<S>,
	calls: &[&str],
	samples: &[NodeSample],
) -> anyhow::Result<Vec<FinalStep>> {
	let single = calls.len() == 1;
	let finals: Vec<FinalStep> = tracker
		.steps
		.iter()
		.zip(calls)
		.flat_map(|(steps, call)| {
			steps.iter().map(move |s| {
				let mut f = final_step(s, (!single).then_some(*call));
				f.node = NodeMax::over(samples, s.started_at, s.ended_at);
				f
			})
		})
		.collect();
	let mut out = dir.jsonl("steps.jsonl")?;
	for f in &finals {
		out.write(f)?;
	}
	out.finish()?;
	Ok(finals)
}

/// 1 in smoke mode when a monitor had a problem or a check that must have a result has none.
fn exit_code(mode: Mode, summary: &Summary, checks: &[polkameter_checks::CheckResult]) -> i32 {
	if mode != Mode::Smoke {
		return 0;
	}
	let mut errors = summary.problems.clone();
	errors.extend(polkameter_checks::smoke_gaps(checks));
	for e in &errors {
		eprintln!("smoke: {e}");
	}
	i32::from(!errors.is_empty())
}

/// `polkameter report <dir>`: run.om and the checks again, from the files alone.
pub fn check(dir: &Path) -> anyhow::Result<()> {
	let run = RunDir::open(dir);
	let summary: Summary =
		serde_json::from_str(&std::fs::read_to_string(dir.join("summary.json"))?)?;
	let finals: Vec<FinalStep> = run.read_jsonl("steps.jsonl")?;
	let plugin_checks: Vec<polkameter_checks::CheckResult> =
		match std::fs::read_to_string(dir.join(PLUGIN_CHECKS)) {
			Ok(text) => serde_json::from_str(&text)?,
			Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
			Err(e) => return Err(e.into()),
		};
	polkameter_checks::report::write(&run, summary, &finals, &plugin_checks)?;
	Ok(())
}

struct PluginState {
	plugins: Plugins,
	operation: String,
	target: String,
	state: serde_json::Value,
	context: Context,
}
impl StateCheck for PluginState {
	fn check<'a>(
		&'a self,
		_client: &'a Client,
		included: &'a [TxHash],
		at: [u8; 32],
	) -> polkameter_load::scenario::BoxFuture<
		'a,
		Result<polkameter_files::summary::StateSample, ChainError>,
	> {
		Box::pin(async move {
			let result = self.plugins.invoke(&self.operation,serde_json::json!({"target":self.target,"state":self.state,"hashes":included.iter().map(hex::encode).collect::<Vec<_>>(),"at":hex::encode(at)}),&self.context,60_000,&CancellationToken::new()).await.map_err(|e| ChainError::Read{what:"plugin state check",detail:e.to_string()})?;
			serde_json::from_value(result).map_err(|e| ChainError::Read {
				what: "plugin state response",
				detail: e.to_string(),
			})
		})
	}
}

#[cfg(test)]
mod tests {
	use super::*;
	use polkameter_checks::{CheckResult, Status, Verdict};

	#[test]
	fn exit_code_policy() {
		let result = |check: &str, status, optional| CheckResult {
			outcome: "pool".into(),
			check: check.into(),
			verdict: Verdict::new(status, check),
			optional,
		};
		for (name, mode, problem, required, optional, expected) in [
			("stress reports health failures", Mode::Stress, false, Status::Fail, Status::Pass, 0),
			("smoke monitor problem", Mode::Smoke, true, Status::Pass, Status::Pass, 1),
			("smoke required gap", Mode::Smoke, false, Status::NoResult, Status::Pass, 1),
			("smoke optional gap", Mode::Smoke, false, Status::Pass, Status::NoResult, 0),
			("smoke all pass", Mode::Smoke, false, Status::Pass, Status::Pass, 0),
		] {
			let mut summary: Summary =
				serde_json::from_str(include_str!("../../checks/tests/summary.json")).unwrap();
			summary.problems = if problem { vec!["monitor scrape failed".into()] } else { vec![] };
			let checks =
				[result("required", required, false), result("from a plugin", optional, true)];
			assert_eq!(exit_code(mode, &summary, &checks), expected, "{name}");
		}
	}
}
