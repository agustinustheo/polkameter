//! Window math and one check on a few hand-written samples.

use super::{end_reasons, outcomes::all, report::overall_status, run, smoke_gaps};
use polkameter_plugin_sdk::observers::{CheckResult, RunData, Status, Verdict};
use polkameter_plugin_sdk::observers::{
	Point, Series, Store, parse_sample_line,
	summary::{OverallStatus, Rule, Summary},
};

const SAMPLES: &str = r#"# TYPE polkameter_step gauge
polkameter_step{instance="load-tool",job="stress"} 0 100.000
polkameter_step{instance="load-tool",job="stress"} 1 110.000
polkameter_step{instance="load-tool",job="stress"} -1 120.000
# TYPE substrate_proposer_block_constructed histogram
substrate_proposer_block_constructed_bucket{instance="c",job="collator",le="1.0"} 0 100.001
substrate_proposer_block_constructed_bucket{instance="c",job="collator",le="2.5"} 0 100.001
substrate_proposer_block_constructed_bucket{instance="c",job="collator",le="+Inf"} 0 100.001
substrate_proposer_block_constructed_bucket{instance="c",job="collator",le="1.0"} 5 110.001
substrate_proposer_block_constructed_bucket{instance="c",job="collator",le="2.5"} 5 110.001
substrate_proposer_block_constructed_bucket{instance="c",job="collator",le="+Inf"} 5 110.001
substrate_proposer_block_constructed_bucket{instance="c",job="collator",le="1.0"} 5 120.001
substrate_proposer_block_constructed_bucket{instance="c",job="collator",le="2.5"} 6 120.001
substrate_proposer_block_constructed_bucket{instance="c",job="collator",le="+Inf"} 10 120.001
# EOF
"#;

/// Parses `name{labels} value timestamp` lines (`#` lines are skipped) into a store.
fn parse_samples(text: &str) -> Store {
	let mut store = Store::new();
	for p in text.lines().filter_map(parse_sample_line) {
		let point = Point { t: p.t.unwrap_or(0.0) * 1000.0, value: p.value };
		let series = store.entry(p.name).or_default();
		match series.iter_mut().find(|s| s.labels == p.labels) {
			Some(s) => s.points.push(point),
			None => series.push(Series { labels: p.labels, points: vec![point] }),
		}
	}
	store
}

fn summary() -> Summary {
	serde_json::from_str(include_str!("../../../tests/fixtures/smoke-run/summary.json")).unwrap()
}

#[test]
fn end_reasons_count_each_block_once_over_collators() {
	let samples = r#"# TYPE polkameter_step gauge
polkameter_step{instance="load-tool",job="stress"} 0 100.000
polkameter_step{instance="load-tool",job="stress"} -1 110.000
# TYPE substrate_proposer_end_proposal_reason counter
substrate_proposer_end_proposal_reason{instance="a",job="collator",reason="no_more_transactions"} 1 100.001
substrate_proposer_end_proposal_reason{instance="b",job="collator",reason="no_more_transactions"} 2 100.001
substrate_proposer_end_proposal_reason{instance="b",job="collator",reason="hit_deadline"} 0 100.001
substrate_proposer_end_proposal_reason{instance="a",job="collator",reason="no_more_transactions"} 4 110.001
substrate_proposer_end_proposal_reason{instance="b",job="collator",reason="no_more_transactions"} 4 110.001
substrate_proposer_end_proposal_reason{instance="b",job="collator",reason="hit_deadline"} 1 110.001
substrate_proposer_end_proposal_reason{instance="a",job="collator",reason="hit_block_weight_limit"} 2 110.001
# EOF
"#;
	let d = RunData::new(parse_samples(samples), summary());
	let mut reasons = end_reasons(&d, &d.steps()[0]).unwrap();
	reasons.sort_by(|a, b| a.0.cmp(&b.0));
	// weight: a's series first appears at the end scrape, after a was scraped without it: 0 -> 2.
	assert_eq!(
		reasons,
		vec![("deadline".to_owned(), 1.0), ("empty".to_owned(), 5.0), ("weight".to_owned(), 2.0)]
	);
}

#[test]
fn a_state_check_without_a_result_makes_the_verdict_inconclusive() {
	use polkameter_plugin_sdk::observers::summary::{OverallStatus, Rule, Stop};
	let mut s = summary();
	// A clean run: every check passes but the plan's state check timed out.
	s.breaking_point = None;
	s.stop = Stop::new(Rule::RateCap, Some(2), "all 3 steps ran");
	s.loss.state = None;
	s.loss.state_error = Some("plugin state check: timed out after 60 s".into());
	let d = RunData::new(Store::new(), s.clone());
	let results = run(&all(), &d);
	let loss = results.iter().find(|r| r.check == "no silent loss").unwrap();
	assert_eq!(loss.verdict.status, Status::NoResult, "{}", loss.verdict.detail);
	assert!(!loss.optional);
	assert_ne!(overall_status(&s, &results), OverallStatus::Pass);
	// The same run with the state check's result: the loss check passes.
	s.loss.state_error = None;
	let results = run(&all(), &RunData::new(Store::new(), s));
	let loss = results.iter().find(|r| r.check == "no silent loss").unwrap();
	assert_eq!(loss.verdict.status, Status::Pass);
}

#[test]
fn blocks_unobserved_for_longer_than_a_stall_are_no_result() {
	use polkameter_plugin_sdk::observers::summary::{OverallStatus, Rule, Stop};
	let mut s = summary();
	s.breaking_point = None;
	s.stop = Stop::new(Rule::RateCap, Some(2), "all 3 steps ran");
	s.problems = vec!["blocks: RPC connection lost (x); reconnected after 14.0 s".into(); 3];
	let check = |s: &Summary| {
		let results = run(&all(), &RunData::new(Store::new(), s.clone()));
		let r = results.iter().find(|r| r.check == "monitors recorded everything").unwrap();
		(r.verdict.status, overall_status(s, &results))
	};
	// 14 s not observed: within the 30 s stall limit, a warning.
	s.unobserved_ms = 14_000;
	assert_eq!(check(&s).0, Status::Warn);
	// 42 s not observed: a stall could hide in it, so no result, and not a PASS.
	s.unobserved_ms = 42_000;
	let (status, overall) = check(&s);
	assert_eq!(status, Status::NoResult);
	assert_ne!(overall, OverallStatus::Pass);
}

/// The verdict of the built-in check `name` on `samples`.
fn verdict(samples: &str, name: &str) -> Verdict {
	let d = RunData::new(parse_samples(samples), summary());
	run(&all(), &d).into_iter().find(|r| r.check == name).unwrap().verdict
}

const BUILD: &str = "build time within the authoring deadline";

/// Steps starting at each of `edges` (the last one ends the ramp); a collator scraped every
/// `every` s from 100.001 s until `until`. A scrape at a time in `failed` fails, one in `skip`
/// left no record. `blocks(t)` gives the blocks built by `t`: (fast, slow), fast ones under 1 s,
/// slow ones over 2.5 s.
fn collator(
	edges: &[f64],
	every: f64,
	until: f64,
	failed: &[f64],
	skip: &[(f64, f64)],
	blocks: impl Fn(f64) -> (f64, f64),
) -> String {
	let mut text = String::new();
	for (i, t) in edges.iter().enumerate() {
		let step = if i + 1 == edges.len() { -1 } else { i as i64 };
		text.push_str(&format!(
			"polkameter_step{{instance=\"load-tool\",job=\"stress\"}} {step} {t:.3}\n"
		));
	}
	let (mut t, mut failures) = (100.001, 0);
	while t <= until {
		if failed.iter().any(|f| (f - t).abs() < 0.01) {
			failures += 1;
			text.push_str(&format!(
				"polkameter_scrape_failed_total{{instance=\"c\",job=\"collator\"}} {failures} {t:.3}\n"
			));
		} else if !skip.iter().any(|(a, b)| t > *a && t < *b) {
			let (fast, slow) = blocks(t);
			for (le, n) in [("1.0", fast), ("2.5", fast), ("+Inf", fast + slow)] {
				text.push_str(&format!(
					"substrate_proposer_block_constructed_bucket{{instance=\"c\",job=\"collator\",le=\"{le}\"}} {n} {t:.3}\n"
				));
			}
		}
		t += every;
	}
	text
}

/// One fast block every 5 s from 100 s.
fn steady(t: f64) -> (f64, f64) {
	(((t - 100.0) / 5.0).floor(), 0.0)
}

/// Five blocks over 2.5 s in step 0 (100-120 s), none after.
fn slow_then_none(t: f64) -> (f64, f64) {
	(0.0, ((t - 100.0) / 4.0).floor().min(5.0))
}

#[test]
fn build_time_fails_on_slow_blocks_and_never_passes_on_missing_data() {
	let edges = [100.0, 120.0, 140.0];
	// The step edges of the next cases, over the build-time histogram (+Inf count per scrape).
	let reset = build_samples(&[(100.001, 50.0), (110.0, 0.0), (120.001, 60.0), (140.001, 64.0)]);
	// Step 0 resets; step 1's four blocks all took over 2.5 s.
	let mut reset_then_slow = build_samples(&[]);
	for (t, fast, slow) in
		[(100.001, 50.0, 0.0), (110.0, 0.0, 0.0), (120.001, 6.0, 0.0), (140.001, 6.0, 4.0)]
	{
		for (le, n) in [("1.0", fast), ("2.5", fast), ("+Inf", fast + slow)] {
			reset_then_slow.push_str(&format!(
				"substrate_proposer_block_constructed_bucket{{instance=\"c\",job=\"collator\",le=\"{le}\"}} {n} {t}\n"
			));
		}
	}
	let late = |t: f64| (steady(t.max(120.0)).0 - 4.0, 0.0);
	let flat = |t: f64| (steady(t.min(125.0)).0, 0.0);
	let gap = [(126.0, 164.0)];
	for (case, samples, status) in [
		("most blocks over 2.5 s", SAMPLES.to_owned(), Status::Fail),
		("fresh scrapes with blocks", collator(&edges, 5.0, 145.0, &[], &[], steady), Status::Pass),
		// The scrapes stop after 125 s: step 1's end would take the 125 s value.
		("a stale edge scrape", collator(&edges, 5.0, 125.0, &[], &[], steady), Status::NoResult),
		// No scrape around step 1's end: a flat counter still has its edge value, a moving
		// one does not.
		("flat across a gap", collator(&edges, 5.0, 170.0, &[], &gap, flat), Status::Pass),
		("moving across a gap", collator(&edges, 5.0, 170.0, &[], &gap, steady), Status::NoResult),
		// A measured failure stands when another window has no data or is stale.
		("slow, then none", collator(&edges, 5.0, 145.0, &[], &[], slow_then_none), Status::Fail),
		("slow, then stale", collator(&edges, 5.0, 125.0, &[], &[], slow_then_none), Status::Fail),
		// The collator answers but builds nothing in a window: no p95 from zero builds.
		("no build in step 0", collator(&edges, 5.0, 145.0, &[], &[], late), Status::NoResult),
		(
			"no build at all",
			collator(&edges, 5.0, 145.0, &[], &[], |_| (3.0, 0.0)),
			Status::NoResult,
		),
		("a counter reset", reset, Status::NoResult),
		("a reset, then slow blocks", reset_then_slow, Status::Fail),
	] {
		let v = verdict(&samples, BUILD);
		assert_eq!(v.status, status, "{case}: {}", v.detail);
	}
}

#[test]
fn failed_scrapes_and_counter_resets_are_found() {
	// 5 s and 15 s scrape intervals; the first scrape after each edge fails, so the next good
	// one lands a whole interval late. The limit is twice the interval plus the 5 s timeout.
	for every in [5.0, 15.0] {
		let edges = [100.0, 160.0, 220.0];
		let failed = [100.001, 160.001, 220.001];
		let samples = collator(&edges, every, 260.0, &failed, &[], steady);
		assert_eq!(verdict(&samples, BUILD).status, Status::Pass, "every {every} s");
		let d = RunData::new(parse_samples(&samples), summary());
		assert_eq!(d.edge_lag_ms("collator", "c"), 2.0 * every * 1000.0 + 5000.0);
	}

	let mut samples = build_samples(&[(100.001, 0.0), (120.001, 4.0), (140.001, 8.0)]);
	samples.push_str(
		"polkameter_scrape_failed_total{instance=\"c\",job=\"collator\"} 1 125.000\n\
		 polkameter_scrape_failed_total{instance=\"c\",job=\"collator\"} 2 130.000\n\
		 polkameter_scrape_failed_total{instance=\"v\",job=\"validator\"} 1 105.000\n",
	);
	assert_eq!(verdict(&samples, BUILD).status, Status::Pass);
	let d = RunData::new(parse_samples(&samples), summary());
	let steps = d.steps();
	assert_eq!(d.failed_scrapes(&[("job", "collator")], &steps[1]), 2.0);
	assert_eq!(d.failed_scrapes(&[("job", "collator")], &steps[0]), 0.0);
	assert_eq!(d.failed_scrapes(&[], &steps[0]), 1.0);
	assert_eq!(d.gap("substrate_proposer_block_constructed_bucket", &[], &steps[1]), None);

	// 100 -> 0 -> 150: the ends alone give a plausible increase of 50.
	let d = counter(&[(100.001, 100.0), (110.0, 0.0), (120.001, 150.0), (140.001, 160.0)]);
	let steps = d.steps();
	assert!(d.increase("x_total", &[], &steps[0]).is_err());
	// Step 1 starts on the 120 s scrape, after the reset: its increase stands.
	assert_eq!(d.increase("x_total", &[], &steps[1]).unwrap(), 10.0);
	// A reset between the edge scrape and the next is inside the window too.
	let d = counter(&[(100.001, 100.0), (105.0, 3.0), (120.001, 9.0), (140.001, 160.0)]);
	assert!(d.increase("x_total", &[], &d.steps()[0]).is_err());
	// A reset before the window's first scrape is not.
	let d = counter(&[(90.0, 100.0), (95.0, 1.0), (100.001, 2.0), (120.001, 9.0)]);
	assert_eq!(d.increase("x_total", &[], &d.steps()[0]).unwrap(), 7.0);
}

/// Two 20 s steps; the collator's build-time histogram, `+Inf` count per scrape time.
fn build_samples(scrapes: &[(f64, f64)]) -> String {
	let mut text = String::from(
		"polkameter_step{instance=\"load-tool\",job=\"stress\"} 0 100.000\n\
		 polkameter_step{instance=\"load-tool\",job=\"stress\"} 1 120.000\n\
		 polkameter_step{instance=\"load-tool\",job=\"stress\"} -1 140.000\n",
	);
	for (t, n) in scrapes {
		for le in ["1.0", "2.5", "+Inf"] {
			text.push_str(&format!(
				"substrate_proposer_block_constructed_bucket{{instance=\"c\",job=\"collator\",le=\"{le}\"}} {n} {t}\n"
			));
		}
	}
	text
}

/// A counter of one collator over two 20 s steps, its value per scrape time.
fn counter(points: &[(f64, f64)]) -> RunData {
	let mut text = String::from(
		"polkameter_step{instance=\"load-tool\",job=\"stress\"} 0 100.000\n\
		 polkameter_step{instance=\"load-tool\",job=\"stress\"} 1 120.000\n\
		 polkameter_step{instance=\"load-tool\",job=\"stress\"} -1 140.000\n",
	);
	for (t, v) in points {
		text.push_str(&format!("x_total{{instance=\"c\",job=\"collator\"}} {v} {t}\n"));
	}
	RunData::new(parse_samples(&text), summary())
}

/// A run with a baseline, one 30 s step and recovery, and refusals by reason.
fn refusals(reasons: &[(&str, f64)]) -> String {
	let mut text = String::from(
		"polkameter_phase{instance=\"load-tool\",job=\"stress\",phase=\"baseline\"} 1 100.000\n\
		 polkameter_phase{instance=\"load-tool\",job=\"stress\",phase=\"baseline\"} 0 130.000\n",
	);
	for (reason, n) in reasons {
		text.push_str(&format!(
			"polkameter_tx_rejected_total{{call=\"remark\",instance=\"load-tool\",job=\"stress\",reason=\"{reason}\"}} {n} 120.000\n"
		));
	}
	text
}

#[test]
fn refusals_are_clean_only_with_an_error_code() {
	// The load tool labels a refusal whose JSON-RPC error has no code with `0`.
	for (reasons, status) in [
		(&[("1010", 3.0)][..], Status::Pass),
		(&[("1010", 3.0), ("0", 2.0)][..], Status::Fail),
		(&[("no code", 1.0)][..], Status::Fail),
	] {
		let v = verdict(&refusals(reasons), "refusals are clean");
		assert_eq!(v.status, status, "{reasons:?}: {}", v.detail);
	}
}

/// Baseline, one 30 s step and recovery, with the relay recorder's counters per time.
fn relay(finalized: &[(f64, f64)], slots: &[(f64, f64)]) -> String {
	let mut text = String::from(
		"polkameter_phase{instance=\"load-tool\",job=\"stress\",phase=\"baseline\"} 1 100.000\n\
		 polkameter_phase{instance=\"load-tool\",job=\"stress\",phase=\"baseline\"} 0 130.000\n\
		 polkameter_step{instance=\"load-tool\",job=\"stress\"} 0 130.000\n\
		 polkameter_step{instance=\"load-tool\",job=\"stress\"} -1 160.000\n\
		 polkameter_phase{instance=\"chain-recorder\",job=\"stress\",phase=\"done\"} 1 160.000\n",
	);
	for (t, n) in finalized {
		text.push_str(&format!(
			"polkameter_relay_finalized_blocks_total{{instance=\"chain-recorder\",job=\"stress\"}} {n} {t}\n"
		));
	}
	for (t, n) in slots {
		for name in ["polkameter_para_slots_total", "polkameter_para_included_total"] {
			text.push_str(&format!(
				"{name}{{instance=\"chain-recorder\",job=\"stress\",para=\"1502\"}} {n} {t}\n"
			));
		}
	}
	text
}

const RELAY_SLOTS: &str = "relay slots (level 1)";
const TIMEOUTS: &str = "no timed-out candidates or disputes";

#[test]
fn relay_checks_need_finalized_relay_blocks() {
	let both = |points: &[(f64, f64)]| relay(points, points);
	let mut timed_out = relay(&[(100.0, 3.0), (159.0, 3.0)], &[]);
	timed_out.push_str(
		"polkameter_para_timed_out_total{instance=\"chain-recorder\",job=\"stress\",para=\"1502\"} 0 101.000\n\
		 polkameter_para_timed_out_total{instance=\"chain-recorder\",job=\"stress\",para=\"1502\"} 1 140.000\n\
		 polkadot_parachain_candidate_disputes_total{instance=\"v\",job=\"validator\"} 0 100.500\n",
	);
	for (case, samples, check, status) in [
		(
			"relay blocks in every window",
			both(&[(101.0, 1.0), (129.0, 5.0), (159.0, 10.0)]),
			RELAY_SLOTS,
			Status::Pass,
		),
		// The recorder reads a relay block after finality: lag can leave step 0 without one.
		(
			"finality lag in one window",
			both(&[(101.0, 1.0), (129.0, 5.0)]),
			RELAY_SLOTS,
			Status::Pass,
		),
		// Nothing in the baseline: no idle rate, so any missed slot counts.
		("an idle baseline", both(&[(100.0, 0.0), (159.0, 10.0)]), RELAY_SLOTS, Status::Pass),
		("an idle baseline", both(&[(100.0, 0.0), (159.0, 10.0)]), TIMEOUTS, Status::Pass),
		(
			"no finalized relay block",
			relay(&[(100.0, 3.0), (159.0, 3.0)], &[(100.0, 2.0), (159.0, 2.0)]),
			RELAY_SLOTS,
			Status::NoResult,
		),
		(
			"no finalized relay block",
			relay(&[(100.0, 3.0), (159.0, 3.0)], &[(100.0, 2.0), (159.0, 2.0)]),
			TIMEOUTS,
			Status::NoResult,
		),
		// A timed-out candidate fails even with stale validator metrics.
		("a timed-out candidate", timed_out, TIMEOUTS, Status::Fail),
	] {
		let v = verdict(&samples, check);
		assert_eq!(v.status, status, "{case}, {check}: {}", v.detail);
	}
}

/// Every check's result on `samples` with the plan's `<monitors>` (and so its para id) or not.
fn results(samples: &str, monitors: bool) -> Vec<CheckResult> {
	let mut s = summary();
	if !monitors {
		s.network.para_id = None;
	}
	run(&all(), &RunData::new(parse_samples(samples), s))
}

#[test]
fn unmonitored_nodes_are_optional_and_silent_ones_are_required() {
	let all = results(&refusals(&[("1010", 1.0)]), false);
	let get = |name: &str| all.iter().find(|r| r.check == name).unwrap();
	for name in [
		BUILD,
		"why blocks end",
		"pool work leaves time for blocks",
		"PVF time on validators (level 2, all parachains)",
		"collation funnel",
		RELAY_SLOTS,
		TIMEOUTS,
	] {
		let r = get(name);
		assert_eq!(r.verdict.status, Status::NoResult, "{name}");
		assert!(r.verdict.detail.starts_with("not applicable:"), "{name}");
		assert!(r.optional, "{name}");
	}
	assert!(smoke_gaps(&all).iter().all(|g| !g.contains("not applicable")));
	assert!(!get("refusals are clean").optional);
	// Not-applicable checks alone never make a clean run INCONCLUSIVE.
	let mut s = summary();
	s.network.para_id = None;
	s.stop.rule = Rule::RateCap;
	s.stop.class = None;
	s.breaking_point = None;
	s.failure_modes.clear();
	let na: Vec<_> = all
		.into_iter()
		.filter(|r| r.verdict.detail.starts_with("not applicable:"))
		.collect();
	assert_eq!(na.len(), 7);
	assert_eq!(overall_status(&s, &na), OverallStatus::Pass);

	// A monitored node that never answered, and the relay recorder that runs with
	// <monitors>: required checks with no result.
	let samples = "polkameter_scrape_failed_total{instance=\"c\",job=\"collator\"} 1 100.000\n";
	let all = results(samples, true);
	for name in [BUILD, RELAY_SLOTS] {
		let r = all.iter().find(|r| r.check == name).unwrap();
		assert_eq!(r.verdict.status, Status::NoResult, "{name}");
		assert!(!r.optional, "{name}");
	}
}
