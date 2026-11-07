//! The measured load: baseline, rate steps, recovery and reconciliation of prepared transactions,
//! with the node monitors. It returns the summary; the caller writes the report once plugin checks
//! have run.
use crate::engine::{
	execute::{RunOptions as ExitPolicy, Values, resolve, target_endpoint, transactions},
	machine,
	plan::Plan,
	plugins::{Plugins, Registry},
	wiring::{Chains, Monitors, monitor_targets},
};
use crate::load::runner::{Io, Mode, RunOptions, baseline, drain, load as apply_load, recover};
use crate::load::sender::Sender;
use crate::load::steps::final_step;
use crate::load::tracker::{Phase, Tracker};
use crate::load::{Lane, QueueSource, StateCheck, Tx, TxHash, follower, now_ms, rules};
use crate::redact::{Redactor, diag, records};
use anyhow::{Context as _, Result, bail};
use polkameter_plugin_sdk::chain::{ChainError, Client};
use polkameter_plugin_sdk::observers::checks::{CheckResult, Status, report::overall_status};
use polkameter_plugin_sdk::observers::files::summary::{
	Artifact, Loss, LostTx, MaxSustained, Network, NodePool, OverallStatus, Rule, SCHEMA_VERSION,
	Summary,
};
use polkameter_plugin_sdk::observers::files::{FinalStep, NodeMax, NodeSample, RunDir};
use polkameter_plugin_sdk::{Context, StateCheckInput};
use std::fmt::Write as _;
use std::path::Path;
use tokio_util::sync::CancellationToken;

/// A finished measurement, before the report.
pub struct Measured {
	summary: Summary,
	finals: Vec<FinalStep>,
	mode: Mode,
}

impl Measured {
	/// Writes summary.json, redacted, for the plugin checks, which read it with the raw files
	/// before the report adds the checks.
	pub fn write_summary(&self, dir: &RunDir, redactor: &Redactor) -> Result<()> {
		dir.write_json("summary.json", &redactor.json(&self.summary)?)?;
		Ok(())
	}

	/// What the run shows but does not count as a problem, for the event log.
	pub fn warnings(&self) -> &[String] {
		&self.summary.warnings
	}
}

/// File of the plugin check results, kept so `polkameter report` can write the same summary.
const PLUGIN_CHECKS: &str = "plugin-checks.json";

/// How the measurement reports to the run around it.
pub struct Reporting<'a> {
	/// Seconds the run spent before the measurement, for the summary.
	pub setup_seconds: u64,
	/// Called with each phase's name as it starts.
	pub progress: &'a (dyn Fn(&str) -> Result<()> + Send + Sync),
	/// The run's redactor: every record and the summary are redacted with it.
	pub redactor: &'a Redactor,
}

pub async fn run(
	plan: &Plan,
	plugins: &Plugins,
	registry: &Registry,
	values: &Values,
	context: &Context,
	reporting: Reporting<'_>,
) -> Result<Measured> {
	let Reporting { setup_seconds, progress, redactor } = reporting;
	let dir = &RunDir::open(&context.artifact_dir);
	// Every record is redacted as it is written: RPC errors, metric text and chain errors may
	// show a credential or an endpoint's key.
	let jsonl = |name: &str| dir.jsonl(name).map(|writer| records(writer, redactor));
	let load = plan.load.as_ref().context("load missing")?;
	let configured_rules = plan.thresholds.rules()?;
	let mode = plan.mode;
	let url = target_endpoint(plan, &load.target)?.to_owned();
	let client = Client::connect(&url).await?;
	let chain = client.chain_info().await?;
	let block_interval_s = load.block_interval_seconds;
	let flood = transactions(&resolve(values, &load.source)?, &context.artifact_dir)?;
	let probes = transactions(&resolve(values, &load.probes_ref)?, &context.artifact_dir)?;
	anyhow::ensure!(
		probes.len()
			>= crate::load::probe_count(
				load.baseline_probes,
				load.recovery_seconds,
				block_interval_s
			),
		"insufficient probe reserve"
	);
	let lanes = vec![Lane { call: "prepared", source: prepared_source(&flood, &probes)? }];
	let state_check = load
		.state
		.as_ref()
		.map(|state| -> Result<_> {
			Ok(PluginState {
				plugins: plugins.clone(),
				operation: state.check.clone(),
				target: url.clone(),
				state: resolve(values, &state.reference)?,
				context: context.clone(),
			})
		})
		.transpose()?;
	let relay_url = plan
		.monitors
		.as_ref()
		.map(|m| target_endpoint(plan, &m.relay_target).map(str::to_owned))
		.transpose()?;
	let targets = monitor_targets(plan, registry)?.unwrap_or_default();
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
	let chains = Chains { node_url: &url, relay_url: relay_url.as_deref() };
	let mut monitors = Monitors::start(dir, targets, &events, chains, redactor).await?;
	let (replies_tx, replies) = tokio::sync::mpsc::unbounded_channel();
	let (blocks_tx, blocks) = tokio::sync::mpsc::unbounded_channel();
	let sender = Sender::open(&url, load.connections, replies_tx).await?;
	let follow = CancellationToken::new();
	let _follow_guard = follow.clone().drop_guard();
	follower::start(
		client.clone(),
		url.clone(),
		client.normal_limit().await?,
		blocks_tx,
		follow.clone(),
	);
	let mut tracker = Tracker::new(
		lanes,
		sender,
		jsonl("load.jsonl")?,
		jsonl("blocks.jsonl")?,
		load.baseline_probes,
		now_ms(),
	);
	tracker.redact_with(redactor.clone());
	let mut io = Io {
		replies,
		blocks,
		events,
		problems: monitors.problems.clone(),
		tool_failed: monitors.tool_failed.clone(),
		boot: crate::load::clock::boot_ms,
		restarts: monitors.node_restarts.clone(),
	};

	let phases = async {
		progress("baseline")?;
		let base = baseline(&mut tracker, &mut io, &run_opts).await?;
		let threshold_ms = (2 * base.max_ms).max((2000.0 * block_interval_s) as u64);
		progress("load")?;
		let stop = apply_load(&mut tracker, &mut io, &run_opts).await?;
		diag!("stop: {}: {}", stop.rule.name(), stop.detail);
		progress("recovery")?;
		let recovery_from = now_ms();
		let recovery = recover(&mut tracker, &mut io, &run_opts, threshold_ms).await?;
		let recovery_to = now_ms();
		// Every block the follower still has queued, so no inclusion is missed by the loss check.
		if !drain(&mut tracker, &mut io, 30_000).await? {
			diag!(
				"drain: block {} not read after 30 s (last read {})",
				tracker.last_head.1,
				tracker.last_fetched
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
	// The run window ends here: scrape now, so it ends on fresh samples.
	monitors.scraper.scrape_now().await;
	// The node's CPU and memory per step come from node.jsonl, complete once the sampler stopped.
	monitors.stop_sampling().await?;
	let samples: Vec<NodeSample> = dir.read_jsonl("node.jsonl")?;
	recovery.node = NodeMax::over(&samples, recovery_window.0, recovery_window.1);
	let finals = write_steps(dir, &tracker, &samples, redactor)?;
	progress("reconciliation")?;
	// The connection of the start may have dropped during the run (the follower and the sender
	// reconnect on their own): the loss check reads on a live one.
	let client = match client.best_number().await {
		Ok(_) => client,
		Err(e) => {
			diag!("loss check: the RPC connection dropped ({e}); reconnecting");
			Client::connect(&url).await.unwrap_or(client)
		},
	};
	let (loss, lost) = loss_check(
		&client,
		&monitors,
		&mut tracker,
		dir,
		configured_rules.finality_wait_ms,
		state_check.as_ref().map(|s| s as &dyn crate::load::StateCheck),
		redactor,
	)
	.await;
	let mut lost_out = jsonl("lost.jsonl")?;
	for l in &lost {
		lost_out.write(l)?;
	}
	lost_out.flush()?;
	let unreadable = tracker.unreadable.clone();
	let gaps: Vec<String> = tracker.observation_gaps.iter().map(ToString::to_string).collect();
	let unobserved_ms = tracker.unobserved_ms(now_ms());
	// A node that went down is a problem too: smoke mode exits 1 on it. A reconnect to another
	// node is a warning only: behind a load balancer it is a healthy node.
	let node_down = tracker.node_down.clone().map(|why| format!("node down: {why}"));
	let warnings = tracker.warnings.clone();
	tracker.finish()?.close();
	monitors.stop_chain().await?;
	let mut problems = monitors.problems.all();
	problems.extend(unreadable.into_iter().map(|u| format!("blocks: {u}")));
	problems.extend(gaps);
	problems.extend(node_down);
	monitors.stop().await?;

	// Summary.
	let bp = rules::breaking_point(&finals, &configured_rules);
	let sustained = match &bp {
		None => finals.last(),
		Some(b) => finals.iter().find(|f| f.step + 1 == b.step),
	};
	let summary = Summary {
		schema_version: SCHEMA_VERSION,
		scenario: plan.name.clone(),
		artifact: Some(Artifact {
			name: "prepared extrinsics".into(),
			description: format!(
				"Each operation is prepared by an installed plugin and submitted by {}",
				crate::PRODUCT_NAME
			),
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
		warnings,
		unobserved_ms,
		network: Network {
			// The URL may carry an API key in its query or user information.
			url: crate::redact::endpoint(&url),
			para_id: plan.monitors.as_ref().map(|m| m.para_id),
			spec_version: chain.spec_version,
			block_interval_s: (block_interval_s * 100.0).round() / 100.0,
		},
		runner: machine::runner(),
		steps: run_opts.plan.steps.len(),
		checks: None,
		verdict: None,
	};
	Ok(Measured { summary, finals, mode })
}

/// The lane's source from the prepared flood and probe transactions. Each is decoded, and two
/// with the same bytes are refused wherever they are (flood or probes), whatever the spelling
/// of their hash (`0x` prefix or not, letter case): the tracker keys every tx by its 32-byte hash.
fn prepared_source(
	flood: &[polkameter_plugin_sdk::PreparedTx],
	probes: &[polkameter_plugin_sdk::PreparedTx],
) -> Result<QueueSource> {
	let mut seen = std::collections::HashSet::new();
	let mut decode = |txs: &[polkameter_plugin_sdk::PreparedTx], what: &str| {
		txs.iter()
			.enumerate()
			.map(|(i, t)| {
				let tx = Tx::new(t.decode()?);
				anyhow::ensure!(
					seen.insert(tx.hash),
					"duplicate transaction in flood/probe sources: {what} {i} has hash 0x{} again",
					hex::encode(tx.hash)
				);
				Ok(tx)
			})
			.collect::<Result<Vec<_>>>()
	};
	let flood = decode(flood, "flood transaction")?;
	let probes = decode(probes, "probe transaction")?;
	Ok(QueueSource::new(flood, probes, "prepared transactions"))
}

/// Writes the checks (the plugins' after the built-in ones), summary.json and summary.md,
/// and returns how the run exits.
pub fn report(
	dir: &RunDir,
	measured: Measured,
	plugin_checks: &[polkameter_plugin_sdk::observers::checks::CheckResult],
	policy: ExitPolicy,
	redactor: &Redactor,
) -> Result<Exit> {
	dir.write_json(PLUGIN_CHECKS, &redactor.json(&plugin_checks)?)?;
	let Measured { summary, finals, mode } = measured;
	// Every text is redacted before summary.md and summary.json are written or printed.
	let checks = polkameter_plugin_sdk::observers::checks::report::write_redacted(
		dir,
		summary.clone(),
		&finals,
		plugin_checks,
		&|text| redactor.text(text),
	)?;
	let exit = exit_code(mode, &summary, &checks, policy);
	// The reason quotes stop details, problems and check names, which may show a secret.
	Ok(Exit { reason: exit.reason.map(|reason| redactor.text(&reason)), ..exit })
}

/// The loss check, with the node's pool counts read from the collator we submit to after the
/// finality wait (the loss check awaits the read then).
async fn loss_check<S: crate::load::submit::Submit>(
	client: &Client,
	monitors: &Monitors,
	tracker: &mut Tracker<S>,
	dir: &RunDir,
	finality_wait_ms: u64,
	state: Option<&dyn crate::load::StateCheck>,
	redactor: &Redactor,
) -> (Loss, Vec<LostTx>) {
	let scraper = monitors.scraper.clone();
	let node_pool = async move {
		scraper.scrape_now().await;
		let s = scraper.collator.borrow().clone()?;
		Some(NodePool {
			mempool: s.sum("substrate_sub_txpool_unwatched_txs")? as u64,
			ready: s.sum("substrate_ready_transactions_number")? as u64,
		})
	};
	let settled = crate::load::loss::Settled {
		last_ours_block: tracker.last_ours_block.max(tracker.last_fetched),
		flood: &tracker.flood,
		included_in: &tracker.included_in,
		first_fetched: tracker.first_fetched,
	};
	let (mut loss, lost, records) =
		crate::load::loss::loss_check(client, settled, node_pool, state, finality_wait_ms).await;
	if let Err(error) = (|| -> Result<()> {
		let mut out = crate::redact::records(dir.jsonl("transactions.jsonl")?, redactor);
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
	diag!("{}", loss_line(&loss));
	let c = &loss.on_chain;
	diag!(
		"loss check on the finalized chain (blocks {}-{}): {} included, {} the tracker missed, {} only on a fork, {} moved",
		c.blocks.0,
		c.blocks.1,
		c.included,
		c.missed,
		c.only_on_fork,
		c.moved
	);
	(loss, lost)
}

/// The loss check's line on standard error. A count that was not counted (`None`) reads
/// "not counted", never Rust's `Some(…)`/`None`.
fn loss_line(loss: &Loss) -> String {
	let count = |n: Option<u64>, what: &str| match n {
		Some(n) => format!("{n} {what}"),
		None => format!("{what}: not counted"),
	};
	format!(
		"loss check: {} sent, {} included, {} refused, {}, {}",
		loss.sent,
		loss.included,
		loss.refused,
		count(loss.in_pool, "in the pool"),
		count(loss.lost, "lost")
	)
}

/// steps.jsonl: one record per step, with the node's highest CPU and memory while the step ran.
fn write_steps<S: crate::load::submit::Submit>(
	dir: &RunDir,
	tracker: &Tracker<S>,
	samples: &[NodeSample],
	redactor: &Redactor,
) -> Result<Vec<FinalStep>> {
	let finals: Vec<FinalStep> = tracker
		.steps
		.iter()
		.flatten()
		.map(|s| {
			let mut f = final_step(s, None);
			f.node = NodeMax::over(samples, s.started_at, s.ended_at);
			f
		})
		.collect();
	let mut out = records(dir.jsonl("steps.jsonl")?, redactor);
	for f in &finals {
		out.write(f)?;
	}
	out.finish()?;
	Ok(finals)
}

/// How a measured run exits: 1 for an opted-in FAIL or INCONCLUSIVE verdict, or smoke-mode
/// monitor problems and missing required results, with the reason.
#[derive(Debug, Default, PartialEq)]
pub struct Exit {
	pub code: i32,
	/// Why `code` is 1: the smoke problems or the verdict and the flag that failed the run.
	/// `execution.json` keeps it as `exit_reason`.
	pub reason: Option<String>,
}

/// The run's exit, by its mode, verdict and exit policy. Smoke problems are printed as found.
fn exit_code(mode: Mode, summary: &Summary, checks: &[CheckResult], policy: ExitPolicy) -> Exit {
	let mut smoke = Vec::new();
	if mode == Mode::Smoke {
		smoke.clone_from(&summary.problems);
		smoke.extend(polkameter_plugin_sdk::observers::checks::smoke_gaps(checks));
	}
	for e in &smoke {
		diag!("smoke: {e}");
	}
	let reason = exit_reason(summary, checks, policy, &smoke);
	Exit { code: i32::from(reason.is_some()), reason }
}

/// Why the run exits 1, or `None` when it exits 0. A flag is blamed only when it decides the exit:
/// smoke mode fails on its problems whatever the flags, and changing a flag would not help.
fn exit_reason(
	summary: &Summary,
	checks: &[CheckResult],
	policy: ExitPolicy,
	smoke: &[String],
) -> Option<String> {
	let verdict = verdict_failure(summary, checks, policy);
	if smoke.is_empty() {
		let (verdict, flag) = verdict?;
		return Some(format!(
			"verdict {verdict}: {}; exit 1 because of {flag} (the checks are in summary.md)",
			verdict_reason(summary, checks)
		));
	}
	let mut reason = format!(
		"smoke mode fails the run on monitor problems and required checks with no result, \
		 whatever the --fail-on flags; fix {}: {}",
		if smoke.len() == 1 { "this one" } else { "these" },
		listed(smoke.iter().map(String::as_str))
	);
	// A FAIL verdict is a separate cause: dropping --fail-on-verdict alone would not pass the run.
	if let Some(("FAIL", flag)) = verdict {
		write!(
			reason,
			"; verdict FAIL also fails it because of {flag}: {}",
			verdict_reason(summary, checks)
		)
		.unwrap();
	}
	reason.push_str(" (the checks are in summary.md)");
	Some(reason)
}

/// The verdict and the flag when the exit policy fails the run on its verdict, or `None` when
/// it does not.
fn verdict_failure(
	summary: &Summary,
	checks: &[CheckResult],
	policy: ExitPolicy,
) -> Option<(&'static str, &'static str)> {
	match overall_status(summary, checks) {
		OverallStatus::Fail if policy.fail_on_verdict => Some(("FAIL", "--fail-on-verdict")),
		OverallStatus::Inconclusive if policy.fail_on_inconclusive => {
			Some(("INCONCLUSIVE", "--fail-on-inconclusive"))
		},
		_ => None,
	}
}

/// The first [`REASON_CHECKS`] items, then how many more.
fn listed<'a>(items: impl ExactSizeIterator<Item = &'a str>) -> String {
	let total = items.len();
	let shown: Vec<&str> = items.take(REASON_CHECKS).collect();
	let mut list = shown.join(", ");
	if total > shown.len() {
		write!(list, " and {} more", total - shown.len()).unwrap();
	}
	list
}

/// Most check names a verdict reason lists before counting the rest.
const REASON_CHECKS: usize = 5;

/// What made the verdict FAIL or INCONCLUSIVE, by the rules of [`overall_status`].
pub fn verdict_reason(summary: &Summary, checks: &[CheckResult]) -> String {
	let names = |what: &str, wanted: &dyn Fn(&CheckResult) -> bool| {
		let matching: Vec<&str> =
			checks.iter().filter(|c| wanted(c)).map(|c| c.check.as_str()).collect();
		(!matching.is_empty()).then(|| format!("{what}: {}", listed(matching.into_iter())))
	};
	let stop = &summary.stop;
	let mut reasons = Vec::new();
	if overall_status(summary, checks) == OverallStatus::Fail {
		if stop.class.is_some() {
			reasons.push(format!("the load stopped on \"{}\" ({})", stop.rule.name(), stop.detail));
		}
		reasons.extend(
			summary.failure_modes.iter().map(|mode| format!("failure mode: {}", mode.what)),
		);
		if let Some(point) = &summary.breaking_point {
			reasons.push(format!(
				"step {} broke its {} measure ({})",
				point.step, point.measure, point.detail
			));
		}
		reasons.extend(names("failed checks", &|c| c.verdict.status == Status::Fail));
	} else {
		if stop.rule != Rule::RateCap {
			reasons.push(format!(
				"the load ended on \"{}\" before reaching its rate cap ({})",
				stop.rule.name(),
				stop.detail
			));
		}
		reasons.extend(names("required checks with no result", &|c| {
			c.verdict.status == Status::NoResult && !c.optional
		}));
	}
	if reasons.is_empty() { "no reason recorded".into() } else { reasons.join("; ") }
}

/// `polkameter report <dir>`: the checks again, from the files alone. The files were redacted
/// when the run wrote them; `redactor` (the credentials available now) redacts the summary again
/// as it is written.
pub fn check(dir: &Path, redactor: &Redactor) -> anyhow::Result<()> {
	let run = RunDir::open(dir);
	let summary: Summary =
		serde_json::from_str(&std::fs::read_to_string(dir.join("summary.json"))?)?;
	let finals: Vec<FinalStep> = run.read_jsonl("steps.jsonl")?;
	let plugin_checks: Vec<polkameter_plugin_sdk::observers::checks::CheckResult> =
		match std::fs::read_to_string(dir.join(PLUGIN_CHECKS)) {
			Ok(text) => serde_json::from_str(&text)?,
			Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
			Err(e) => return Err(e.into()),
		};
	polkameter_plugin_sdk::observers::checks::report::write_redacted(
		&run,
		summary,
		&finals,
		&plugin_checks,
		&|text| redactor.text(text),
	)?;
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
	) -> crate::load::scenario::BoxFuture<
		'a,
		Result<polkameter_plugin_sdk::observers::files::summary::StateSample, ChainError>,
	> {
		Box::pin(async move {
			let read = |detail: String| ChainError::Read { what: "plugin state check", detail };
			let input = StateCheckInput {
				target: self.target.clone(),
				state: self.state.clone(),
				hashes: included.iter().map(hex::encode).collect(),
				at: hex::encode(at),
			};
			let inputs = serde_json::to_value(input).map_err(|e| read(e.to_string()))?;
			let result = self
				.plugins
				.invoke(&self.operation, inputs, &self.context, 60_000, &CancellationToken::new())
				.await
				.map_err(|e| read(e.to_string()))?;
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
	use polkameter_plugin_sdk::observers::checks::{CheckResult, Status, Verdict};

	#[test]
	fn the_loss_line_prints_counts_not_options() {
		let mut loss = Loss::default();
		(loss.sent, loss.included, loss.refused) = (4500, 4500, 0);
		(loss.in_pool, loss.lost) = (Some(0), Some(0));
		assert_eq!(
			loss_line(&loss),
			"loss check: 4500 sent, 4500 included, 0 refused, 0 in the pool, 0 lost"
		);
		(loss.in_pool, loss.lost) = (None, None);
		let line = loss_line(&loss);
		assert_eq!(
			line,
			"loss check: 4500 sent, 4500 included, 0 refused, in the pool: not counted, lost: not counted"
		);
		assert!(!line.contains("Some(") && !line.contains("None"));
	}

	#[test]
	fn the_same_transaction_spelled_twice_is_a_duplicate() {
		use polkameter_plugin_sdk::PreparedTx;
		let a = PreparedTx::new(&[1, 2, 3], serde_json::Value::Null);
		let b = PreparedTx::new(&[4, 5, 6], serde_json::Value::Null);
		let prefixed = PreparedTx { hash: format!("0x{}", a.hash), ..a.clone() };
		let byte_prefixed = PreparedTx { bytes: format!("0x{}", a.bytes), ..a.clone() };
		assert!(prepared_source(&[a.clone(), b.clone()], &[]).is_ok());
		for (flood, probes) in [
			(vec![a.clone(), prefixed.clone()], vec![]),
			(vec![a.clone(), byte_prefixed], vec![]),
			(vec![a.clone()], vec![prefixed]),
			(vec![b.clone()], vec![b.clone()]),
		] {
			let error = prepared_source(&flood, &probes).unwrap_err().to_string();
			assert!(error.contains("duplicate transaction"), "{error}");
		}
	}

	/// Every exit policy: neither flag, `--fail-on-verdict`, `--fail-on-inconclusive`, both.
	const POLICIES: [ExitPolicy; 4] = [
		ExitPolicy { fail_on_verdict: false, fail_on_inconclusive: false },
		ExitPolicy { fail_on_verdict: true, fail_on_inconclusive: false },
		ExitPolicy { fail_on_verdict: false, fail_on_inconclusive: true },
		ExitPolicy { fail_on_verdict: true, fail_on_inconclusive: true },
	];

	/// The recorded summary with a clean stop: the verdict follows the checks alone.
	fn clean_summary() -> Summary {
		let mut summary: Summary =
			serde_json::from_str(include_str!("../../tests/fixtures/smoke-run/summary.json"))
				.unwrap();
		summary.breaking_point = None;
		summary.problems.clear();
		summary
	}

	fn plugin_check(name: &str, status: Status, optional: bool) -> CheckResult {
		CheckResult {
			source: None,
			outcome: "run".into(),
			check: name.into(),
			verdict: Verdict::new(status, "test"),
			optional,
		}
	}

	#[test]
	fn each_policy_fails_only_its_own_verdict() {
		for mode in [Mode::Stress, Mode::Smoke] {
			for (status, verdict) in [
				(Status::Pass, OverallStatus::Pass),
				(Status::Warn, OverallStatus::Warn),
				(Status::Fail, OverallStatus::Fail),
				(Status::NoResult, OverallStatus::Inconclusive),
			] {
				for policy in POLICIES {
					let summary = clean_summary();
					let checks = [plugin_check("plugin check", status, false)];
					assert_eq!(overall_status(&summary, &checks), verdict);
					let by_policy = (verdict == OverallStatus::Fail && policy.fail_on_verdict)
						|| (verdict == OverallStatus::Inconclusive && policy.fail_on_inconclusive);
					// Smoke mode already fails a required check with no result.
					let by_smoke = mode == Mode::Smoke && status == Status::NoResult;
					let exit = exit_code(mode, &summary, &checks, policy);
					assert_eq!(
						exit.code,
						i32::from(by_policy || by_smoke),
						"{mode:?} {verdict:?} {policy:?}"
					);
					assert_eq!(
						verdict_failure(&summary, &checks, policy).is_some(),
						by_policy,
						"{mode:?} {verdict:?} {policy:?}"
					);
					// The flag is blamed only when it decides the exit.
					let reason = exit.reason.unwrap_or_default();
					assert_eq!(
						reason.contains("exit 1 because of"),
						by_policy && !by_smoke,
						"{mode:?} {verdict:?} {policy:?}: {reason}"
					);
					assert_eq!(reason.starts_with("smoke mode"), by_smoke, "{reason}");
				}
			}
		}
	}

	#[test]
	fn inconclusive_reason_names_the_stop_and_the_required_checks_without_results() {
		let policy = ExitPolicy { fail_on_verdict: false, fail_on_inconclusive: true };
		let mut summary = clean_summary();
		let checks: Vec<CheckResult> = (1..=7)
			.map(|i| plugin_check(&format!("required {i}"), Status::NoResult, false))
			.chain([
				plugin_check("optional", Status::NoResult, true),
				plugin_check("passed", Status::Pass, false),
			])
			.collect();
		assert_eq!(
			exit_code(Mode::Stress, &summary, &checks, policy).reason.unwrap(),
			"verdict INCONCLUSIVE: required checks with no result: required 1, required 2, \
			 required 3, required 4, required 5 and 2 more; exit 1 because of \
			 --fail-on-inconclusive (the checks are in summary.md)"
		);
		summary.stop = polkameter_plugin_sdk::observers::files::summary::Stop::new(
			Rule::BudgetUsedUp,
			Some(1),
			"all 60 prepared transactions sent (13 more kept for probes)",
		);
		let checks = [plugin_check("passed", Status::Pass, false)];
		assert_eq!(overall_status(&summary, &checks), OverallStatus::Inconclusive);
		assert_eq!(
			verdict_reason(&summary, &checks),
			"the load ended on \"budget used up\" before reaching its rate cap (all 60 prepared \
			 transactions sent (13 more kept for probes))"
		);
		assert_eq!(exit_code(Mode::Stress, &summary, &checks, policy).code, 1);
		assert_eq!(
			exit_code(Mode::Stress, &summary, &checks, ExitPolicy::default()),
			Exit::default()
		);
	}

	#[test]
	fn a_monitorless_run_passes_every_exit_policy() {
		// The recorded run without what only monitors write (scrapes.jsonl, chain.jsonl) and
		// with no parachain: the checks of unmonitored nodes are optional, so neither
		// --fail-on-inconclusive nor smoke mode fails it.
		let fixture = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/smoke-run");
		let root = std::env::temp_dir().join(format!(
			"polkameter-monitorless-policy-{}-{}",
			std::process::id(),
			std::time::SystemTime::now()
				.duration_since(std::time::UNIX_EPOCH)
				.unwrap()
				.as_nanos()
		));
		std::fs::create_dir_all(&root).unwrap();
		for name in ["load.jsonl", "steps.jsonl", "transactions.jsonl"] {
			std::fs::copy(fixture.join(name), root.join(name)).unwrap();
		}
		let dir = RunDir::open(&root);
		for mode in [Mode::Stress, Mode::Smoke] {
			for policy in POLICIES {
				let mut summary = clean_summary();
				summary.network.para_id = None;
				summary.checks = None;
				summary.verdict = None;
				let finals = dir.read_jsonl("steps.jsonl").unwrap();
				let measured = Measured { summary, finals, mode };
				assert_eq!(
					report(&dir, measured, &[], policy, &Redactor::default()).unwrap(),
					Exit::default(),
					"{mode:?} {policy:?}"
				);
				let json: serde_json::Value =
					serde_json::from_slice(&std::fs::read(root.join("summary.json")).unwrap())
						.unwrap();
				assert_ne!(json["verdict"]["result"], "INCONCLUSIVE", "{mode:?} {policy:?}");
			}
		}
		std::fs::remove_dir_all(root).unwrap();
	}

	#[test]
	fn fail_reason_names_the_failed_checks() {
		let checks = [
			plugin_check("too slow", Status::Fail, false),
			plugin_check("fine", Status::Pass, false),
		];
		assert_eq!(verdict_reason(&clean_summary(), &checks), "failed checks: too slow");
		let mut summary = clean_summary();
		summary.stop = polkameter_plugin_sdk::observers::files::summary::Stop::new(
			Rule::Stall,
			Some(2),
			"no block for 30 s",
		);
		assert_eq!(
			verdict_reason(&summary, &[]),
			"the load stopped on \"stall\" (no block for 30 s)"
		);
	}

	#[test]
	fn recorded_fail_report_changes_exit_code_only_when_opted_in() {
		// Replay the exit policy with the real report's summary and checks, without replacing
		// verdicts or inventing failures. The CLI replay test separately regenerates these checks
		// from the raw recorded files; `report` itself has no exit policy options.
		let summary: Summary =
			serde_json::from_str(include_str!("../../tests/fixtures/smoke-run/summary.json"))
				.unwrap();
		let checks: Vec<CheckResult> =
			serde_json::from_value(summary.checks.clone().unwrap()).unwrap();
		assert!(checks.iter().any(|check| {
			check.check == "relay slots (level 1)" && check.verdict.status == Status::Fail
		}));
		assert_eq!(
			polkameter_plugin_sdk::observers::checks::report::overall_status(&summary, &checks),
			polkameter_plugin_sdk::observers::files::summary::OverallStatus::Fail
		);
		for mode in [Mode::Smoke, Mode::Stress] {
			for policy in POLICIES {
				assert_eq!(
					exit_code(mode, &summary, &checks, policy).code,
					i32::from(policy.fail_on_verdict),
					"{mode:?} {policy:?}"
				);
			}
		}
	}

	#[test]
	fn plugin_check_details_and_endpoints_are_redacted_before_the_report_renders_them() {
		let root =
			std::env::temp_dir().join(format!("polkameter-redacted-report-{}", std::process::id()));
		std::fs::create_dir_all(&root).unwrap();
		let dir = RunDir::open(&root);
		// A `|` is escaped in the Markdown table, so redacting only the rendered text would miss
		// this secret.
		let secret = "pipe|secret\"value";
		let endpoint = "wss://rpc.example/ws?apikey=APIKEY0123456789"; // gitleaks:allow (made-up test key)
		let redactor = Redactor::new([secret], [endpoint]);
		let mut summary: Summary =
			serde_json::from_str(include_str!("../../tests/fixtures/smoke-run/summary.json"))
				.unwrap();
		summary.network.url = endpoint.into();
		summary.problems = vec![format!("cannot reach {endpoint}")];
		let check = CheckResult {
			source: Some("evaluate".into()),
			outcome: "plugin".into(),
			check: format!("uses {secret}"),
			verdict: Verdict::new(Status::Fail, format!("detail {secret} at {endpoint}"))
				.with(serde_json::json!({ "key": secret })),
			optional: false,
		};
		let measured = Measured { summary, finals: vec![], mode: Mode::Smoke };
		measured.write_summary(&dir, &redactor).unwrap();
		let early = std::fs::read_to_string(root.join("summary.json")).unwrap();
		assert!(!early.contains("APIKEY"), "{early}");
		let exit = report(&dir, measured, &[check], ExitPolicy::default(), &redactor).unwrap();
		assert_eq!(exit.code, 1);
		let reason = exit.reason.unwrap();
		assert!(reason.starts_with("smoke mode") && !reason.contains("APIKEY"), "{reason}");
		for name in ["summary.md", "summary.json", PLUGIN_CHECKS] {
			let text = std::fs::read_to_string(root.join(name)).unwrap();
			for leak in ["secret", "APIKEY"] {
				assert!(!text.contains(leak), "{name} shows {leak}: {text}");
			}
		}
		let md = std::fs::read_to_string(root.join("summary.md")).unwrap();
		assert!(md.contains("| plugin | uses [redacted] | **fail** | detail [redacted] at wss://rpc.example/ws?apikey=[redacted] |"), "{md}");
		std::fs::remove_dir_all(root).unwrap();
	}

	#[test]
	fn report_writes_the_same_fail_verdict_with_either_exit_policy() {
		let root =
			std::env::temp_dir().join(format!("polkameter-verdict-report-{}", std::process::id()));
		std::fs::create_dir_all(&root).unwrap();
		let dir = RunDir::open(&root);
		for policy in POLICIES {
			let summary: Summary =
				serde_json::from_str(include_str!("../../tests/fixtures/smoke-run/summary.json"))
					.unwrap();
			let measured = Measured { summary, finals: vec![], mode: Mode::Stress };
			assert_eq!(
				report(&dir, measured, &[], policy, &Redactor::default()).unwrap().code,
				i32::from(policy.fail_on_verdict)
			);
			let json: serde_json::Value =
				serde_json::from_slice(&std::fs::read(root.join("summary.json")).unwrap()).unwrap();
			assert_eq!(json["verdict"]["result"], "FAIL");
			assert!(
				std::fs::read_to_string(root.join("summary.md"))
					.unwrap()
					.contains("- **Verdict:** FAIL")
			);
		}
		std::fs::remove_dir_all(root).unwrap();
	}

	#[test]
	fn report_from_the_files_redacts_the_summary_it_writes() {
		let root =
			std::env::temp_dir().join(format!("polkameter-offline-report-{}", std::process::id()));
		std::fs::create_dir_all(&root).unwrap();
		let mut summary: Summary =
			serde_json::from_str(include_str!("../../tests/fixtures/smoke-run/summary.json"))
				.unwrap();
		// A file written before redaction covered it, or by something else.
		summary.scenario = "Plan OfflineSecret-0123".into();
		RunDir::open(&root).write_json("summary.json", &summary).unwrap();
		check(&root, &Redactor::new(["OfflineSecret-0123"], None::<&str>)).unwrap();
		for name in ["summary.md", "summary.json"] {
			let text = std::fs::read_to_string(root.join(name)).unwrap();
			assert!(!text.contains("OfflineSecret"), "{name}: {text}");
			assert!(text.contains("Plan [redacted]"), "{name}: {text}");
		}
		std::fs::remove_dir_all(root).unwrap();
	}

	#[test]
	fn exit_code_policy() {
		let result = |check: &str, status, optional| CheckResult {
			source: None,
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
				serde_json::from_str(include_str!("../../tests/fixtures/smoke-run/summary.json"))
					.unwrap();
			summary.problems = if problem { vec!["monitor scrape failed".into()] } else { vec![] };
			let checks =
				[result("required", required, false), result("from a plugin", optional, true)];
			assert_eq!(
				exit_code(mode, &summary, &checks, ExitPolicy::default()).code,
				expected,
				"{name}"
			);
		}
	}

	#[test]
	fn smoke_failures_do_not_blame_the_flags() {
		let both = ExitPolicy { fail_on_verdict: true, fail_on_inconclusive: true };
		let mut summary = clean_summary();
		let gap = [plugin_check("no silent loss", Status::NoResult, false)];
		// INCONCLUSIVE because a required check has no result: smoke mode fails on that anyway.
		for policy in POLICIES {
			let exit = exit_code(Mode::Smoke, &summary, &gap, policy);
			assert_eq!(
				exit,
				Exit {
					code: 1,
					reason: Some(
						"smoke mode fails the run on monitor problems and required checks with no \
						 result, whatever the --fail-on flags; fix this one: no silent loss: test \
						 (the checks are in summary.md)"
							.into()
					),
				},
				"{policy:?}"
			);
		}
		// A monitor problem and a FAIL verdict with --fail-on-verdict: both causes are named.
		summary.problems = vec!["monitor scrape failed".into()];
		let failed = [plugin_check("too slow", Status::Fail, false)];
		let reason = exit_code(Mode::Smoke, &summary, &failed, both).reason.unwrap();
		assert_eq!(
			reason,
			"smoke mode fails the run on monitor problems and required checks with no result, \
			 whatever the --fail-on flags; fix this one: monitor scrape failed; verdict FAIL also \
			 fails it because of --fail-on-verdict: failed checks: too slow (the checks are in \
			 summary.md)"
		);
		// Smoke mode with a stop that is not caught by smoke: the flag decides, and is blamed.
		summary.problems.clear();
		summary.stop = polkameter_plugin_sdk::observers::files::summary::Stop::new(
			Rule::GeneratorLimit,
			Some(0),
			"sent 50 of 100 tx/s",
		);
		let passed = [plugin_check("fine", Status::Pass, false)];
		let exit = exit_code(Mode::Smoke, &summary, &passed, both);
		assert_eq!(exit.code, 1);
		assert!(
			exit.reason.unwrap().ends_with(
				"exit 1 because of --fail-on-inconclusive (the checks are in summary.md)"
			)
		);
		assert_eq!(
			exit_code(Mode::Smoke, &summary, &passed, ExitPolicy::default()),
			Exit::default()
		);
	}
}
