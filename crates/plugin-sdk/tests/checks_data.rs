//! Window math and one check on a few hand-written samples.

use polkameter_plugin_sdk::observers::checks::{self, RunData, Status, Verdict};
use polkameter_plugin_sdk::observers::files::{
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
	serde_json::from_str(include_str!("../../cli/tests/fixtures/smoke-run/summary.json")).unwrap()
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
	let mut reasons = d.end_reasons(&d.steps()[0]).unwrap();
	reasons.sort_by(|a, b| a.0.cmp(&b.0));
	// weight: a's series first appears at the end scrape, after a was scraped without it: 0 -> 2.
	assert_eq!(
		reasons,
		vec![("deadline".to_owned(), 1.0), ("empty".to_owned(), 5.0), ("weight".to_owned(), 2.0)]
	);
}

#[test]
fn build_time_fails_when_most_blocks_take_over_2_5_s() {
	let d = RunData::new(parse_samples(SAMPLES), summary());
	let results = polkameter_plugin_sdk::observers::checks::run(
		&polkameter_plugin_sdk::observers::checks::all(),
		&d,
	);
	let build = results
		.iter()
		.find(|r| r.check == "build time within the authoring deadline")
		.unwrap();
	assert_eq!(build.verdict.status, Status::Fail, "{}", build.verdict.detail);
	assert!(build.verdict.detail.starts_with("step 1: 4 of 5 blocks took over 2.5 s"));
}

/// The verdict of the built-in check `name` on `samples`.
fn verdict(samples: &str, name: &str) -> Verdict {
	let d = RunData::new(parse_samples(samples), summary());
	checks::run(&checks::all(), &d)
		.into_iter()
		.find(|r| r.check == name)
		.unwrap()
		.verdict
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

#[test]
fn fresh_scrapes_with_blocks_pass_build_time() {
	let v = verdict(&collator(&[100.0, 120.0, 140.0], 5.0, 145.0, &[], &[], steady), BUILD);
	assert_eq!(v.status, Status::Pass, "{}", v.detail);
}

#[test]
fn a_stale_edge_scrape_gives_build_time_no_result() {
	// The scrapes stop after 125 s: step 1's end (140 s) would take the 125 s value, so the
	// step showed fewer blocks and passed before.
	let v = verdict(&collator(&[100.0, 120.0, 140.0], 5.0, 125.0, &[], &[], steady), BUILD);
	assert_eq!(v.status, Status::NoResult, "{}", v.detail);
	assert_eq!(
		v.detail,
		"stale node metrics: step 1: no scrape of collator c within 15 s after its end"
	);
}

#[test]
fn one_failed_scrape_at_every_edge_keeps_the_result() {
	// 5 s and 15 s scrape intervals; the first scrape after each edge fails, so the next good
	// one lands a whole interval late. The limit is twice the interval plus the 5 s timeout.
	for every in [5.0, 15.0] {
		let edges = [100.0, 160.0, 220.0];
		let failed = [100.001, 160.001, 220.001];
		let samples = collator(&edges, every, 260.0, &failed, &[], steady);
		let v = verdict(&samples, BUILD);
		assert_eq!(v.status, Status::Pass, "every {every} s: {}", v.detail);
		assert!(v.detail.contains("failed scrapes: 1 in step 0, 1 in step 1"), "{}", v.detail);
		let d = RunData::new(parse_samples(&samples), summary());
		assert_eq!(d.edge_lag_ms("collator", "c"), 2.0 * every * 1000.0 + 5000.0);
	}
}

#[test]
fn a_counter_flat_across_a_long_gap_still_has_its_edge_value() {
	// No scrape from 125 s to 165 s around step 1's end (140 s): 25 s late, over the 15 s limit.
	let flat = |t: f64| (steady(t.min(125.0)).0, 0.0);
	let samples = collator(&[100.0, 120.0, 140.0], 5.0, 170.0, &[], &[(126.0, 164.0)], flat);
	let v = verdict(&samples, BUILD);
	// The count is the same on both sides of the gap, so its value at 140 s is exact.
	assert_eq!(v.status, Status::Pass, "{}", v.detail);
	let samples = collator(&[100.0, 120.0, 140.0], 5.0, 170.0, &[], &[(126.0, 164.0)], steady);
	let v = verdict(&samples, BUILD);
	assert_eq!(v.status, Status::NoResult, "{}", v.detail);
}

/// Five blocks over 2.5 s in step 0 (100-120 s), none after.
fn slow_then_none(t: f64) -> (f64, f64) {
	(0.0, ((t - 100.0) / 4.0).floor().min(5.0))
}

#[test]
fn a_measured_failure_stands_when_another_window_has_no_data() {
	// Step 1 is fresh but built nothing: no data for it, and step 0 still fails.
	let samples = collator(&[100.0, 120.0, 140.0], 5.0, 145.0, &[], &[], slow_then_none);
	let v = verdict(&samples, BUILD);
	assert_eq!(v.status, Status::Fail, "{}", v.detail);
	assert_eq!(
		v.detail,
		"step 0: 5 of 5 blocks took over 2.5 s (deadline 2 s); no data for step 1: the collator recorded no built block"
	);
	// Step 1's end is stale: the same.
	let samples = collator(&[100.0, 120.0, 140.0], 5.0, 125.0, &[], &[], slow_then_none);
	let v = verdict(&samples, BUILD);
	assert_eq!(v.status, Status::Fail, "{}", v.detail);
	assert!(v.detail.contains("no data for stale node metrics: step 1"), "{}", v.detail);
}

#[test]
fn no_built_block_in_a_long_window_is_no_result_not_a_pass() {
	// The collator answers, but its counter does not move in step 0: p95 "at most 1 s" with
	// zero builds.
	let late = |t: f64| (steady(t.max(120.0)).0 - 4.0, 0.0);
	let v = verdict(&collator(&[100.0, 120.0, 140.0], 5.0, 145.0, &[], &[], late), BUILD);
	assert_eq!(v.status, Status::NoResult, "{}", v.detail);
	assert_eq!(v.detail, "step 0: the collator recorded no built block");
	let none = |_: f64| (3.0, 0.0);
	let v = verdict(&collator(&[100.0, 120.0, 140.0], 5.0, 145.0, &[], &[], none), BUILD);
	assert_eq!(v.status, Status::NoResult, "{}", v.detail);
	assert_eq!(
		v.detail,
		"step 0: the collator recorded no built block; step 1: the collator recorded no built block"
	);
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

#[test]
fn failed_scrapes_inside_a_window_show_in_the_check() {
	let mut samples = build_samples(&[(100.001, 0.0), (120.001, 4.0), (140.001, 8.0)]);
	samples.push_str(
		"polkameter_scrape_failed_total{instance=\"c\",job=\"collator\"} 1 125.000\n\
		 polkameter_scrape_failed_total{instance=\"c\",job=\"collator\"} 2 130.000\n\
		 polkameter_scrape_failed_total{instance=\"v\",job=\"validator\"} 1 105.000\n",
	);
	let v = verdict(&samples, BUILD);
	assert_eq!(v.status, Status::Pass, "{}", v.detail);
	assert!(v.detail.ends_with("; failed scrapes: 2 in step 1"), "{}", v.detail);
	let d = RunData::new(parse_samples(&samples), summary());
	let steps = d.steps();
	assert_eq!(d.failed_scrapes(&[("job", "collator")], &steps[1]), 2.0);
	assert_eq!(d.failed_scrapes(&[("job", "collator")], &steps[0]), 0.0);
	assert_eq!(d.failed_scrapes(&[], &steps[0]), 1.0);
	assert_eq!(d.gap("substrate_proposer_block_constructed_bucket", &[], &steps[1]), None);
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

#[test]
fn a_counter_reset_inside_a_window_is_found() {
	// 100 -> 0 -> 150: the ends alone give a plausible increase of 50.
	let d = counter(&[(100.001, 100.0), (110.0, 0.0), (120.001, 150.0), (140.001, 160.0)]);
	let steps = d.steps();
	let reset = d.increase("x_total", &[], &steps[0]).unwrap_err();
	assert_eq!(reset.to_string(), "x_total went down from 100 to 0 in step 0");
	// Step 1 starts on the 120 s scrape, after the reset: its increase stands.
	assert_eq!(d.increase("x_total", &[], &steps[1]).unwrap(), 10.0);
	// A reset between the edge scrape and the next is inside the window too.
	let d = counter(&[(100.001, 100.0), (105.0, 3.0), (120.001, 9.0), (140.001, 160.0)]);
	assert!(d.increase("x_total", &[], &d.steps()[0]).is_err());
	// A reset before the window's first scrape is not.
	let d = counter(&[(90.0, 100.0), (95.0, 1.0), (100.001, 2.0), (120.001, 9.0)]);
	assert_eq!(d.increase("x_total", &[], &d.steps()[0]).unwrap(), 7.0);
}

#[test]
fn a_reset_gives_its_window_no_data_and_a_failure_elsewhere_stands() {
	let v = verdict(
		&build_samples(&[(100.001, 50.0), (110.0, 0.0), (120.001, 60.0), (140.001, 64.0)]),
		BUILD,
	);
	assert_eq!(v.status, Status::NoResult, "{}", v.detail);
	assert!(v.detail.starts_with("step 0: a node restarted: "), "{}", v.detail);
	// Step 1's four blocks all took over 2.5 s: a failure, whatever step 0 lacks.
	let mut samples = String::new();
	for (t, fast, slow) in
		[(100.001, 50.0, 0.0), (110.0, 0.0, 0.0), (120.001, 6.0, 0.0), (140.001, 6.0, 4.0)]
	{
		for (le, n) in [("1.0", fast), ("2.5", fast), ("+Inf", fast + slow)] {
			samples.push_str(&format!(
				"substrate_proposer_block_constructed_bucket{{instance=\"c\",job=\"collator\",le=\"{le}\"}} {n} {t}\n"
			));
		}
	}
	samples.push_str(&build_samples(&[])); // the steps
	let v = verdict(&samples, BUILD);
	assert_eq!(v.status, Status::Fail, "{}", v.detail);
	assert!(v.detail.starts_with("step 1: 4 of 4 blocks took over 2.5 s"), "{}", v.detail);
	assert!(v.detail.contains("; no data for step 0: a node restarted: "), "{}", v.detail);
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
fn refusals_with_an_error_code_are_clean() {
	let v = verdict(&refusals(&[("1010", 3.0)]), "refusals are clean");
	assert_eq!(v.status, Status::Pass, "{}", v.detail);
	assert_eq!(v.detail, "3 refused (1010: 3)");
}

#[test]
fn refusals_without_an_error_code_fail() {
	// The load tool labels a refusal whose JSON-RPC error has no code with `0`.
	let v = verdict(&refusals(&[("1010", 3.0), ("0", 2.0)]), "refusals are clean");
	assert_eq!(v.status, Status::Fail, "{}", v.detail);
	assert_eq!(v.detail, "5 refused (1010: 3, no code: 2)");
	let v = verdict(&refusals(&[("no code", 1.0)]), "refusals are clean");
	assert_eq!(v.status, Status::Fail, "{}", v.detail);
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
fn relay_slots_pass_with_relay_blocks_in_every_window() {
	let samples = relay(
		&[(101.0, 1.0), (129.0, 5.0), (159.0, 10.0)],
		&[(101.0, 1.0), (129.0, 5.0), (159.0, 10.0)],
	);
	let v = verdict(&samples, RELAY_SLOTS);
	assert_eq!(v.status, Status::Pass, "{}", v.detail);
}

#[test]
fn finality_lag_in_one_window_is_not_missing_data() {
	// The recorder reads a relay block, and its slots, after finality: lag can leave step 0
	// without one while the run has plenty.
	let samples = relay(&[(101.0, 1.0), (129.0, 5.0)], &[(101.0, 1.0), (129.0, 5.0)]);
	let v = verdict(&samples, RELAY_SLOTS);
	assert_eq!(v.status, Status::Pass, "{}", v.detail);
	// Nothing in the baseline: no idle rate, so any missed slot counts; the detail says so.
	let samples = relay(&[(100.0, 0.0), (159.0, 10.0)], &[(100.0, 0.0), (159.0, 10.0)]);
	let v = verdict(&samples, RELAY_SLOTS);
	assert_eq!(v.status, Status::Pass, "{}", v.detail);
	assert!(v.detail.ends_with("so idle counts as no missed slot"), "{}", v.detail);
	assert_eq!(verdict(&samples, TIMEOUTS).status, Status::Pass);
}

#[test]
fn a_run_without_finalized_relay_blocks_has_no_result() {
	let samples = relay(&[(100.0, 3.0), (159.0, 3.0)], &[(100.0, 2.0), (159.0, 2.0)]);
	let v = verdict(&samples, RELAY_SLOTS);
	assert_eq!(v.status, Status::NoResult, "{}", v.detail);
	assert_eq!(v.detail, "the relay recorder read no finalized relay block in the run");
	let v = verdict(&samples, TIMEOUTS);
	assert_eq!(v.status, Status::NoResult, "{}", v.detail);
}

#[test]
fn a_timed_out_candidate_fails_even_with_stale_validator_metrics() {
	let mut samples = relay(&[(100.0, 3.0), (159.0, 3.0)], &[]);
	samples.push_str(
		"polkameter_para_timed_out_total{instance=\"chain-recorder\",job=\"stress\",para=\"1502\"} 0 101.000\n\
		 polkameter_para_timed_out_total{instance=\"chain-recorder\",job=\"stress\",para=\"1502\"} 1 140.000\n\
		 polkadot_parachain_candidate_disputes_total{instance=\"v\",job=\"validator\"} 0 100.500\n",
	);
	let v = verdict(&samples, TIMEOUTS);
	assert_eq!(v.status, Status::Fail, "{}", v.detail);
	assert!(v.detail.starts_with("parachain candidates timed out 1;"), "{}", v.detail);
	assert!(
		v.detail
			.contains("no data for stale node metrics: run: no scrape of validator v")
	);
}

/// Every check's result on `samples` with the plan's `<monitors>` (and so its para id) or not.
fn results(samples: &str, monitors: bool) -> Vec<checks::CheckResult> {
	let mut s = summary();
	if !monitors {
		s.network.para_id = None;
	}
	checks::run(&checks::all(), &RunData::new(parse_samples(samples), s))
}

#[test]
fn checks_of_unmonitored_nodes_are_not_applicable_and_optional() {
	let samples = refusals(&[("1010", 1.0)]);
	let all = results(&samples, false);
	let get = |name: &str| all.iter().find(|r| r.check == name).unwrap();
	for (name, why) in [
		(BUILD, "no collator node is monitored"),
		("why blocks end", "no collator node is monitored"),
		("pool work leaves time for blocks", "no collator node is monitored"),
		("PVF time on validators (level 2, all parachains)", "no validator node is monitored"),
		("collation funnel", "no collator-relay node is monitored"),
		(RELAY_SLOTS, "the plan has no <monitors>, so no relay chain is recorded"),
		(TIMEOUTS, "the plan has no <monitors>, so no relay chain is recorded"),
	] {
		let r = get(name);
		assert_eq!(r.verdict.status, Status::NoResult, "{name}");
		assert_eq!(r.verdict.detail, format!("not applicable: {why}"), "{name}");
		assert!(r.optional, "{name}");
	}
	assert!(checks::smoke_gaps(&all).iter().all(|g| !g.contains("not applicable")));
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
	assert_eq!(checks::report::overall_status(&s, &na), OverallStatus::Pass);
}

#[test]
fn a_monitored_node_that_never_answered_is_a_required_no_result() {
	let samples = "polkameter_scrape_failed_total{instance=\"c\",job=\"collator\"} 1 100.000\n";
	let all = results(samples, true);
	let build = all.iter().find(|r| r.check == BUILD).unwrap();
	assert_eq!(build.verdict.status, Status::NoResult);
	assert_eq!(build.verdict.detail, "no collator metrics");
	assert!(!build.optional);
	// The relay recorder runs with <monitors>: its checks stay required too.
	let relay = all.iter().find(|r| r.check == RELAY_SLOTS).unwrap();
	assert_eq!(relay.verdict.detail, "no relay recorder data");
	assert!(!relay.optional);
}
