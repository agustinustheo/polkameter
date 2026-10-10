//! The summary of a run: reads the raw files, runs the checks on them and writes `summary.json`
//! and `summary.md`. The collator table comes from the same series, so the summary and the checks
//! read the same numbers, and `polkameter report <dir>` can write it all again later.

use polkameter_files::{
	BlockStats, FileError, FinalStep, RunDir, num, read_store, serde_name,
	summary::{ProbePhase, Rule, Summary},
	to_fixed,
};
use serde::Serialize;

use crate::{
	CheckResult, RunData, Status, Window, all,
	data::{COLLATOR, END_REASON, Label, Phase, add_to, reasons_text},
	run,
};

/// Runs every check on the run's files, appends the plugins' check results, and writes
/// summary.json (with all checks) and summary.md.
pub fn write(
	dir: &RunDir,
	mut summary: Summary,
	finals: &[FinalStep],
	plugin_checks: &[CheckResult],
) -> Result<Vec<CheckResult>, FileError> {
	let data = RunData::new(read_store(dir)?, summary.clone());
	let mut checks = run(&all(), &data);
	checks.extend_from_slice(plugin_checks);
	summary.checks = Some(serde_json::to_value(&checks).expect("checks serialize"));
	dir.write_json("summary.json", &summary)?;
	let md = markdown(&summary, finals, &data, &checks);
	std::fs::write(dir.path.join("summary.md"), &md).map_err(|source| FileError::Io {
		path: dir.path.join("summary.md").display().to_string(),
		source,
	})?;
	eprintln!("\n{md}");
	Ok(checks)
}

/// The whole summary.md.
fn markdown(s: &Summary, finals: &[FinalStep], d: &RunData, checks: &[CheckResult]) -> String {
	format!(
		"{}\n### Outcome checks\n\n{}\n",
		render_summary(s, finals, d, checks),
		render_checks(checks)
	)
}

/// The checks table.
fn render_checks(results: &[CheckResult]) -> String {
	let mut lines = vec![
		"| outcome | check | status | detail |".to_owned(),
		"| --- | --- | --- | --- |".to_owned(),
	];
	lines.extend(results.iter().map(|r| {
		format!(
			"| {} | {} | **{}** | {} |",
			r.outcome,
			r.check,
			serde_name(r.verdict.status),
			r.verdict.detail.replace('|', "\\|")
		)
	}));
	lines.join("\n")
}

/// `-` for nothing, else the number to `digits` places.
fn fmt(n: Option<f64>, digits: usize) -> String {
	n.map_or("-".into(), |n| to_fixed(n, digits))
}

fn row(cells: &[String]) -> String {
	format!("| {} |", cells.join(" | "))
}

/// The collator in one step: mean build time, pool work, and why blocks ended.
struct CollatorStep {
	build_ms: Option<f64>,
	validations: Option<f64>,
	waiting: Option<f64>,
	ready: Option<f64>,
	end_reasons: Vec<(String, f64)>,
}

/// `None` without collator metrics, or when a counter went down (the node restarted).
fn collator_step(d: &RunData, w: Option<&Window>) -> Option<CollatorStep> {
	let w = w?;
	if !d.has(END_REASON, &[COLLATOR]) {
		return None;
	}
	let at = |name: &str| d.at(name, &[COLLATOR], w.end);
	let builds = d.diff("substrate_proposer_block_constructed_count", &[COLLATOR], w).ok()?;
	let build_s = d.diff("substrate_proposer_block_constructed_sum", &[COLLATOR], w).ok()?;
	let (scheduled, finished) = (
		at("substrate_sub_txpool_validations_scheduled"),
		at("substrate_sub_txpool_validations_finished"),
	);
	Some(CollatorStep {
		build_ms: match (builds, build_s) {
			(Some(b), Some(s)) if b != 0.0 => Some(1000.0 * s / b),
			_ => None,
		},
		validations: d.diff("substrate_sub_txpool_validations_finished", &[COLLATOR], w).ok()?,
		waiting: scheduled.zip(finished).map(|(s, f)| s - f),
		ready: at("substrate_ready_transactions_number"),
		end_reasons: d.end_reasons(w).ok()?,
	})
}

fn block_cells(b: &BlockStats) -> Vec<String> {
	vec![
		b.blocks.to_string(),
		format!(
			"{} / {}",
			fmt(b.mean_block_gap_ms.map(|g| g / 1000.0), 1),
			fmt(b.max_block_gap_ms.map(|g| g as f64 / 1000.0), 1)
		),
		b.max_ours_per_block.to_string(),
		to_fixed(b.max_normal_ref_time_pct, 1),
		to_fixed(b.max_normal_proof_pct, 1),
	]
}

fn collator_cells(c: Option<&CollatorStep>) -> Vec<String> {
	let why = match c {
		Some(c) if c.end_reasons.is_empty() => "-".into(),
		Some(c) => reasons_text(&c.end_reasons),
		None => "no metrics".into(),
	};
	let cell = |value: fn(&CollatorStep) -> Option<f64>| fmt(c.and_then(value), 0);
	vec![cell(|c| c.build_ms), cell(|c| c.validations), cell(|c| c.waiting), cell(|c| c.ready), why]
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "UPPERCASE")]
enum OverallStatus {
	Pass,
	Warn,
	Fail,
	Inconclusive,
}

/// A number from the run's thresholds, if the run has that rule.
fn rule(s: &Summary, key: &str) -> Option<f64> {
	s.rules.get(key).and_then(serde_json::Value::as_f64)
}

/// One top-level answer from the runner's stop, its performance measure and the outcome checks.
fn overall_status(s: &Summary, checks: &[CheckResult]) -> OverallStatus {
	if s.stop.class.is_some()
		|| !s.failure_modes.is_empty()
		|| s.breaking_point.is_some()
		|| checks.iter().any(|r| r.verdict.status == Status::Fail)
	{
		return OverallStatus::Fail;
	}
	if s.stop.rule != Rule::RateCap {
		return OverallStatus::Inconclusive;
	}
	let required_gap = checks.iter().any(|r| r.verdict.status == Status::NoResult && !r.optional);
	if required_gap {
		OverallStatus::Inconclusive
	} else if checks.iter().any(|r| r.verdict.status == Status::Warn) {
		OverallStatus::Warn
	} else {
		OverallStatus::Pass
	}
}

fn recovery_line(s: &Summary) -> String {
	let r = &s.recovery;
	if !r.measured {
		return format!("not measured: {}", r.detail);
	}
	if r.recovered {
		let left = match r.drained_seconds {
			None => format!("{} left", r.backlog_at_end),
			Some(d) => format!("drained after {d} s"),
		};
		format!(
			"back {} s after the load stopped ({}). Backlog at stop {} txs, {left}, at {} tx/s.",
			fmt(r.seconds.map(|x| x as f64), 0),
			r.detail,
			r.backlog_at_stop,
			to_fixed(r.drain_per_s, 1)
		)
	} else {
		format!(
			"not back within the budget: {}. Backlog at stop {} txs, {} left, drained at {} tx/s.",
			r.detail,
			r.backlog_at_stop,
			r.backlog_at_end,
			to_fixed(r.drain_per_s, 1)
		)
	}
}

fn loss_line(s: &Summary) -> String {
	let l = &s.loss;
	let mut out = format!(
		"{} sent: {} finalized ({} failed after inclusion), {} refused, {} still ready in the pool, {} seen in a block but not verified final, **{} lost**, {} unknown",
		l.sent,
		l.included,
		l.failed_in_block,
		l.refused,
		fmt(l.in_pool.map(|x| x as f64), 0),
		l.unverified,
		fmt(l.lost.map(|x| x as f64), 0),
		l.unknown
	);
	if let Some(p) = &l.node_pool {
		out += &format!(" (the node's mempool holds {} txs, {} of them ready)", p.mempool, p.ready);
	}
	if let Some(n) = &l.note {
		out += &format!(" ({n})");
	}
	let c = &l.on_chain;
	out += &format!(
		". On the finalized chain (blocks {}-{}): {} included; {} the tracker missed (in a block that replaced one it read), {} only on a fork block, {} moved to another block",
		c.blocks.0, c.blocks.1, c.included, c.missed, c.only_on_fork, c.moved
	);
	if let Some(st) = &l.state {
		out += &format!(
			". State check: {} of {} {}",
			st.checked.saturating_sub(st.missing),
			st.checked,
			st.detail
		);
	}
	out
}

fn step_row(f: &FinalStep) -> String {
	let mut cells = vec![
		f.step.to_string(),
		num(f.target_rate),
		to_fixed(f.seconds, 1),
		f.sent.to_string(),
		to_fixed(f.sent_per_s, 0),
		to_fixed(f.included_per_s, 0),
		to_fixed(100.0 * f.included_ratio, 1),
		to_fixed(f.p50_latency_ms as f64 / 1000.0, 1),
		to_fixed(f.p95_latency_ms as f64 / 1000.0, 1),
		to_fixed(f.p95_reply_ms as f64 / 1000.0, 2),
		f.rejected.to_string(),
		f.failed_in_block.to_string(),
	];
	cells.extend(block_cells(&f.blocks));
	row(&cells)
}

/// Refusals over every step, by "code message", most first; ties keep the order they were seen in.
fn rpc_errors(steps: &[FinalStep]) -> Vec<(String, u64)> {
	let mut errors: Vec<(String, u64)> = Vec::new();
	for (k, v) in steps.iter().flat_map(|f| &f.errors) {
		add_to(&mut errors, k.clone(), *v);
	}
	errors.sort_by(|a, b| b.1.cmp(&a.1));
	errors
}

#[allow(clippy::too_many_lines)]
fn render_summary(s: &Summary, steps: &[FinalStep], d: &RunData, checks: &[CheckResult]) -> String {
	let r = &s.recovery;
	let bp = s.breaking_point.as_ref().map_or("no measure violated".into(), |b| {
		format!("step {} ({} tx/s): {}, {}", b.step, num(b.target_rate), b.measure, b.detail)
	});
	let best = s.max_sustained.as_ref().map_or("none".into(), |m| {
		format!(
			"{} tx/s target (step {}), {} tx/s included",
			num(m.target_rate),
			m.step,
			to_fixed(m.included_per_s, 0)
		)
	});
	let min_included_pct = rule(s, "minIncludedRatio").map_or(90.0, |v| 100.0 * v);
	let max_latency_s = rule(s, "maxP95LatencyMs").map_or(10.0, |v| v / 1000.0);
	let max_refused_pct = rule(s, "maxRefusedRatio").map_or(1.0, |v| 100.0 * v);
	let windows = d.steps();
	let step_window = |k: u32| windows.iter().find(|w| w.label == Label::Step(k));
	let mut lines: Vec<String> = vec![format!("### {}", s.scenario), String::new()];
	if let Some(artifact) = &s.artifact {
		lines.extend([
			"#### Artifact under test".into(),
			String::new(),
			format!("`{}`", artifact.name),
			String::new(),
			artifact.description.clone(),
			String::new(),
			artifact.context.clone(),
			String::new(),
		]);
	}
	lines.extend([
        "#### Stress method".into(),
        String::new(),
        "The stress test offers unique, prepared Artifact transactions at increasing rates in consecutive fixed-duration steps. The first step that violates a stress criterion is the failure onset.".into(),
        String::new(),
        format!("A step fails the response criteria when fewer than {}% of its transactions are included, p95 send-to-inclusion latency exceeds {} s, or more than {}% of submissions are refused.", to_fixed(min_included_pct, 0), to_fixed(max_latency_s, 1), to_fixed(max_refused_pct, 0)),
        String::new(),
        format!("Scenario setup prepared {}. Setup took {} s and is excluded from the measured load. The baseline then sent {} probes, one per block.", s.budget, s.setup_seconds, s.baseline.probes),
        String::new(),
        "#### Result".into(),
        String::new(),
        format!("- **Verdict:** {}", serde_name(overall_status(s, checks))),
        format!("- **Failure onset** (first failed load step): {bp}"),
        format!("- **Max sustained:** {best}"),
        format!("- **Load stop:** {}: {}", s.stop.rule.name(), s.stop.detail),
        format!("- **Recovery:** {}", recovery_line(s)),
        format!("- **Loss check:** {}", loss_line(s)),
        format!("- **Baseline:** {} probes before the load, p50 {} s, max {} s", s.baseline.probes, to_fixed(s.baseline.p50_ms as f64 / 1000.0, 1), to_fixed(s.baseline.max_ms as f64 / 1000.0, 1)),
        format!("- **Network:** spec {}, {} s blocks at start", s.network.spec_version, num(s.network.block_interval_s)),
        format!("- **Runner:** {} CPUs ({})", s.runner.cpus, s.runner.cpu_model.as_deref().unwrap_or("?")),
        String::new(),
        "#### Load steps".into(),
        String::new(),
        format!("| step | target tx/s | duration s | transactions sent | sent/s | included/s | included % | p50 s | p95 s (limit {}) | reply p95 s | refused | failed | blocks | block gap s (mean / max) | max ours/block | max ref time % | max proof % |", to_fixed(max_latency_s, 1)),
        format!("|{}", " ---: |".repeat(17)),
    ]);
	lines.extend(steps.iter().map(step_row));
	let mut cells = vec!["recovery".to_owned()];
	cells.extend(std::iter::repeat_n("-".to_owned(), 11));
	cells.extend(block_cells(&r.blocks));
	lines.push(row(&cells));
	lines.extend([
        String::new(),
        "Collator metrics per step (CPU/memory: the node under load; block/pool metrics: all collators):".into(),
        String::new(),
        "| step | max CPU % | max memory MiB | block build ms | pool validations (all) | validations waiting (all) | pool ready txs (all) | why blocks ended |".into(),
        format!("|{} --- |", " ---: |".repeat(7)),
    ]);
	// One row per step, then one for recovery.
	let windows = steps
		.iter()
		.map(|f| (f.step.to_string(), &f.node, step_window(f.step).cloned()))
		.chain([("recovery".to_owned(), &r.node, d.phase(Phase::Recovery))]);
	for (label, node, w) in windows {
		let c = collator_step(d, w.as_ref());
		let mut cells = vec![
			label,
			fmt(node.max_cpu_pct.map(|x| x as f64), 0),
			fmt(node.max_rss_mi_b.map(|x| x as f64), 0),
		];
		cells.extend(collator_cells(c.as_ref()));
		lines.push(row(&cells));
	}
	lines.push(String::new());
	let probes: Vec<_> = r.probes.iter().filter(|p| p.phase == ProbePhase::Recovery).collect();
	if !probes.is_empty() {
		let landed: Vec<String> = probes
			.iter()
			.map(|p| {
				format!(
					"{} s: {}",
					to_fixed(p.sent_at_s, 0),
					p.latency_ms.map_or_else(
						|| serde_name(p.outcome),
						|l| format!("{} s", to_fixed(l as f64 / 1000.0, 1))
					)
				)
			})
			.collect();
		lines.extend([
			format!(
				"Recovery probes (one per block; landing within {} s counts as back):",
				to_fixed(r.threshold_ms as f64 / 1000.0, 1)
			),
			String::new(),
			landed.join(", "),
			String::new(),
		]);
	}
	let errors = rpc_errors(steps);
	if !errors.is_empty() {
		lines.extend(["RPC errors:".into(), String::new()]);
		lines.extend(errors.iter().take(5).map(|(k, v)| format!("- {v} × `{k}`")));
		lines.push(String::new());
	}
	lines.join("\n")
}

#[cfg(test)]
mod tests {
	use polkameter_files::summary::{Artifact, BreakingPoint, Stop};

	use super::*;
	use crate::Verdict;

	/// The recorded smoke run, shared with the replay test. Its breaking point is cleared: the
	/// verdict tests set their own.
	fn summary() -> Summary {
		let text = include_str!("../../../src-tauri/tests/fixtures/smoke-run/summary.json");
		let mut s: Summary = serde_json::from_str(text).expect("summary fixture");
		s.breaking_point = None;
		s
	}

	fn check(name: &str, status: Status) -> CheckResult {
		CheckResult {
			outcome: "run".into(),
			check: name.into(),
			verdict: Verdict::new(status, "test"),
			optional: false,
		}
	}

	#[test]
	fn the_overall_status_follows_the_stop_the_breaking_point_and_the_checks() {
		let plain: fn(&mut Summary) = |_| {};
		let breaking: fn(&mut Summary) = |s| {
			s.breaking_point = Some(BreakingPoint {
				step: 1,
				target_rate: 18.0,
				measure: "latency".into(),
				detail: "p95 27.3 s".into(),
			})
		};
		let ended: fn(&mut Summary) =
			|s| s.stop = Stop::new(Rule::BudgetUsedUp, Some(1), "no transactions left");
		let optional = CheckResult { optional: true, ..check("x", Status::NoResult) };
		let cases = [
			(plain, vec![check("x", Status::Pass)], OverallStatus::Pass),
			(plain, vec![check("x", Status::Fail)], OverallStatus::Fail),
			(plain, vec![check("x", Status::Warn)], OverallStatus::Warn),
			(plain, vec![check("x", Status::NoResult)], OverallStatus::Inconclusive),
			(plain, vec![optional], OverallStatus::Pass),
			(breaking, vec![], OverallStatus::Fail),
			(ended, vec![], OverallStatus::Inconclusive),
		];
		for (edit, checks, want) in cases {
			let mut s = summary();
			edit(&mut s);
			assert_eq!(overall_status(&s, &checks), want);
		}
	}

	#[test]
	fn the_summary_separates_the_verdict_from_the_load_stop() {
		let s = summary();
		let data = RunData::new(Default::default(), s.clone());
		let md = markdown(&s, &[], &data, &[check("relay slots (level 1)", Status::Fail)]);
		assert!(md.contains("- **Verdict:** FAIL"));
		assert!(md.contains("- **Load stop:** rate cap"));
		assert!(!md.contains("- **Failure:**"));
		assert!(md.contains("pool validations (all)"));
	}

	#[test]
	fn the_summary_explains_the_artifact_load_and_exact_threshold() {
		let mut s = summary();
		s.artifact = Some(Artifact {
			name: "Balances.transfer_keep_alive(dest, value)".into(),
			description: "One signed transfer per prepared account.".into(),
			context: "Wallets move funds between accounts.".into(),
		});
		let step = FinalStep {
			step: 0,
			target_rate: 15.0,
			seconds: 60.0,
			sent: 900,
			p95_latency_ms: 8_400,
			..FinalStep::default()
		};
		let data = RunData::new(Default::default(), s.clone());
		let md = markdown(&s, &[step], &data, &[]);
		assert!(md.contains("#### Artifact under test"));
		assert!(md.contains("`Balances.transfer_keep_alive(dest, value)`"));
		assert!(md.contains("Wallets move funds between accounts"));
		assert!(md.contains("p95 send-to-inclusion latency exceeds 10.0 s"));
		assert!(md.contains("p95 s (limit 10.0)"));
		assert!(md.contains("| 0 | 15 | 60.0 | 900 |"));
	}
}
