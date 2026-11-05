//! Window math and one check on a few hand-written samples.

use polkameter_plugin_sdk::observers::checks::{self, RunData, Status, Verdict};
use polkameter_plugin_sdk::observers::files::{
	Point, Series, Store, parse_sample_line, summary::Summary,
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
fn fresh_scrapes_with_blocks_pass_build_time() {
	let v = verdict(&build_samples(&[(100.001, 0.0), (120.001, 4.0), (140.001, 8.0)]), BUILD);
	assert_eq!(v.status, Status::Pass, "{}", v.detail);
}

#[test]
fn a_stale_edge_scrape_gives_build_time_no_result() {
	// The scrapes stop after 120 s: step 1's end (140 s) would take the 120 s value, so the
	// step showed no block and passed before.
	let v = verdict(&build_samples(&[(100.001, 0.0), (120.001, 4.0)]), BUILD);
	assert_eq!(v.status, Status::NoResult, "{}", v.detail);
	assert_eq!(
		v.detail,
		"stale node metrics: step 1: no scrape of collator c within 10 s after its end"
	);
	// A later scrape no further than 10 s after the edge still stands for it.
	let v = verdict(&build_samples(&[(100.001, 0.0), (120.001, 4.0), (149.9, 8.0)]), BUILD);
	assert_eq!(v.status, Status::Pass, "{}", v.detail);
	let v = verdict(&build_samples(&[(100.001, 0.0), (120.001, 4.0), (150.5, 8.0)]), BUILD);
	assert_eq!(v.status, Status::NoResult, "{}", v.detail);
}

#[test]
fn no_built_block_in_a_long_window_is_no_result_not_a_pass() {
	// The collator answers, but its counter never moves: p95 "at most 1 s" with zero builds.
	let v = verdict(&build_samples(&[(100.001, 3.0), (120.001, 3.0), (140.001, 7.0)]), BUILD);
	assert_eq!(v.status, Status::NoResult, "{}", v.detail);
	assert_eq!(v.detail, "the collator recorded no built block in step 0");
	let v = verdict(&build_samples(&[(100.001, 3.0), (120.001, 3.0), (140.001, 3.0)]), BUILD);
	assert_eq!(v.status, Status::NoResult, "{}", v.detail);
	assert_eq!(v.detail, "the collator recorded no built block in step 0, step 1");
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
fn a_reset_gives_the_check_no_result() {
	let v = verdict(
		&build_samples(&[(100.001, 50.0), (110.0, 0.0), (120.001, 60.0), (140.001, 64.0)]),
		BUILD,
	);
	assert_eq!(v.status, Status::NoResult, "{}", v.detail);
	assert!(v.detail.starts_with("a node restarted: "), "{}", v.detail);
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
fn relay_slots_without_finalized_relay_blocks_have_no_result() {
	// The recorder lost the relay after the baseline: step 0 saw nothing and passed before.
	let samples = relay(&[(101.0, 1.0), (129.0, 5.0)], &[(101.0, 1.0), (129.0, 5.0)]);
	let v = verdict(&samples, RELAY_SLOTS);
	assert_eq!(v.status, Status::NoResult, "{}", v.detail);
	assert_eq!(v.detail, "the relay recorder read no finalized relay block in step 0");
	// Nothing in the baseline: no idle rate to compare with.
	let samples = relay(&[(100.0, 0.0), (159.0, 10.0)], &[(100.0, 0.0), (159.0, 10.0)]);
	let v = verdict(&samples, RELAY_SLOTS);
	assert_eq!(v.status, Status::NoResult, "{}", v.detail);
	let v = verdict(&samples, "no timed-out candidates or disputes");
	assert_eq!(v.status, Status::Pass, "{}", v.detail);
	let samples = relay(&[(100.0, 3.0), (159.0, 3.0)], &[]);
	let v = verdict(&samples, "no timed-out candidates or disputes");
	assert_eq!(v.status, Status::NoResult, "{}", v.detail);
}
