//! Window math and one check on a few hand-written samples.

use polkameter_plugin_sdk::observers::checks::{RunData, Status};
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

#[test]
fn a_state_check_without_a_result_makes_the_verdict_inconclusive() {
	use polkameter_plugin_sdk::observers::checks::{all, report::overall_status, run};
	use polkameter_plugin_sdk::observers::files::summary::{OverallStatus, Rule, Stop};
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
	assert!(loss.verdict.detail.contains("timed out after 60 s"));
	assert!(!loss.optional);
	assert_ne!(overall_status(&s, &results), OverallStatus::Pass);
	// The same run with the state check's result: the loss check passes.
	s.loss.state_error = None;
	let results = run(&all(), &RunData::new(Store::new(), s));
	let loss = results.iter().find(|r| r.check == "no silent loss").unwrap();
	assert_eq!(loss.verdict.status, Status::Pass);
}
