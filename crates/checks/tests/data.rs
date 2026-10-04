//! Window math and one check on a few hand-written samples.

use polkameter_checks::{RunData, Status, count_above, quantile};
use polkameter_files::parse_samples;
use polkameter_files::summary::Summary;

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

fn summary() -> Summary {
	let text = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/summary.json"))
		.unwrap();
	serde_json::from_str(&text).unwrap()
}

#[test]
fn steps_and_buckets() {
	let d = RunData::new(parse_samples(SAMPLES), summary());
	let steps = d.steps();
	assert_eq!(steps.iter().map(|w| w.label.as_str()).collect::<Vec<_>>(), ["step 0", "step 1"]);
	let b = d
		.buckets("substrate_proposer_block_constructed", &[("job", "collator")], &steps[1])
		.unwrap()
		.unwrap();
	assert_eq!(b, vec![(1.0, 0.0), (2.5, 1.0), (f64::INFINITY, 5.0)]);
	assert_eq!(quantile(Some(&b), 0.95), Some(f64::INFINITY));
	assert_eq!(count_above(Some(&b), 2.5), Some(4.0));
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
	let results = polkameter_checks::run(&polkameter_checks::all(), &d);
	let build = results
		.iter()
		.find(|r| r.check == "build time within the authoring deadline")
		.unwrap();
	assert_eq!(build.verdict.status, Status::Fail, "{}", build.verdict.detail);
	assert!(build.verdict.detail.starts_with("step 1: 4 of 5 blocks took over 2.5 s"));
}
