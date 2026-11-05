//! JUnit reports derived only from recorded events, plan structure and load results.
use std::{
	collections::BTreeMap,
	fmt::Write as _,
	fs,
	io::{BufRead as _, BufReader, Write as _},
	path::Path,
};

use anyhow::Result;
use serde_json::Value;

use crate::engine::execute::{redact, redact_text};

/// Maximum message/name length before XML escaping (entities require at most six bytes).
pub const MESSAGE_CHARS: usize = 2048;
const FAILURES_PER_STEP: usize = 100;
const REPORT_BYTES: usize = 4 * 1024 * 1024;
const XML_CHAR_BYTES: usize = 6;
const SUITE_RESERVE: usize = MESSAGE_CHARS * XML_CHAR_BYTES * 6 + 4096;

/// Best-effort JUnit warning. Never changes an execution outcome, even if event recording fails.
/// Remote clients emit the returned event in their own event stream (no local run directory).
pub fn warning(directory: Option<&Path>, message: &str, secrets: &[String]) -> Value {
	let message = redact_text(&format!("JUnit report: {message}"), secrets);
	eprintln!("Polkameter warning: {}", single_line(&message));
	let event = serde_json::json!({"event":"warning", "message":message,
		"timestamp":polkameter_plugin_sdk::observers::files::now_ms(), "version":2});
	if let Some(directory) = directory {
		let _ = (|| -> Result<()> {
			let mut file =
				fs::OpenOptions::new().append(true).open(directory.join("events.jsonl"))?;
			writeln!(file, "{event}")?;
			Ok(())
		})();
	}
	event
}

#[derive(Debug, PartialEq)]
enum Status {
	Passed,
	Failed,
	Skipped,
}

struct Case {
	name: String,
	ms: u64,
	status: Status,
	message: String,
}

#[derive(Default)]
struct Suite {
	name: String,
	cases: Vec<Case>,
}

type Key = (String, Option<u64>, Option<u64>);

/// Writes `junit.xml`. Run-time callers pass the same secrets used to redact events; offline
/// callers use available credentials and the already-redacted recorded files. Only recorded
/// error summaries (including bounded assertion values), not raw inputs/outputs, are exported.
///
/// `events.jsonl` is read one line at a time and only what the report needs from each step
/// event is kept, so a long run's event log is never held in memory.
pub fn write(directory: &Path, secrets: &[String]) -> Result<()> {
	let mut plan = read_optional(&directory.join("plan.json"))?;
	let mut summary = read_optional(&directory.join("summary.json"))?;
	let mut outcome: Value = serde_json::from_slice(&fs::read(directory.join("execution.json"))?)?;
	for value in [&mut plan, &mut summary, &mut outcome] {
		redact(value, secrets);
	}
	let file = fs::File::open(directory.join("events.jsonl"))?;
	let events = BufReader::new(file).lines().map(|line| -> Result<Value> {
		let mut event = serde_json::from_str(&line?)?;
		redact(&mut event, secrets);
		Ok(event)
	});
	let suites = suites(&plan, events, &summary, &outcome)?;
	let safe_path = redact_text(&directory.display().to_string(), secrets);
	let xml = render(&suites, Path::new(&safe_path));
	fs::write(directory.join("junit.xml"), xml)?;
	Ok(())
}

fn read_optional(path: &Path) -> Result<Value> {
	match fs::read(path) {
		Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
		Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Value::Null),
		Err(error) => Err(error.into()),
	}
}

fn text<'a>(value: &'a Value, key: &str) -> &'a str {
	value[key].as_str().unwrap_or("")
}

fn items(value: &Value) -> &[Value] {
	value.as_array().map_or(&[], Vec::as_slice)
}

fn key(event: &Value) -> Key {
	(text(event, "step").into(), event["user"].as_u64(), event["iteration"].as_u64())
}

/// What the report keeps of a `step-finished` event.
struct Finished {
	success: bool,
	elapsed_ms: u64,
	error: Option<String>,
}

/// When the load measurement started and the last teardown phase began, from phase events.
#[derive(Default)]
struct LoadTiming {
	/// The first `measurement` phase event's timestamp, if there is such an event.
	start: Option<Option<u64>>,
	/// The last `teardown` phase event's timestamp, if there is such an event.
	stop: Option<Option<u64>>,
}

impl LoadTiming {
	fn ms(&self) -> u64 {
		self.start
			.zip(self.stop)
			.map_or(0, |(start, stop)| stop.unwrap_or(0).saturating_sub(start.unwrap_or(0)))
	}
}

/// The entries of one step's invocations, in key order.
fn of_step<'a, T>(
	map: &'a BTreeMap<Key, T>,
	id: &'a str,
) -> impl Iterator<Item = (&'a Key, &'a T)> {
	map.range((id.to_owned(), None, None)..).take_while(move |(key, _)| key.0 == id)
}

/// Builds the suites from the plan and the recorded events, read once in order. Only a small
/// record per step invocation is kept, never the events themselves.
fn suites(
	plan: &Value,
	events: impl IntoIterator<Item = Result<Value>>,
	summary: &Value,
	outcome: &Value,
) -> Result<Vec<Suite>> {
	let mut suites = vec![Suite { name: "plan".into(), cases: vec![] }];
	let mut owners = BTreeMap::new();
	for section in ["preflight", "setup", "evaluate", "teardown"] {
		add_section(&mut suites, &mut owners, section.into(), items(&plan[section]["step"]));
	}
	for workflow in items(&plan["workflow"]) {
		add_section(
			&mut suites,
			&mut owners,
			format!("workflow:{}", text(workflow, "@id")),
			items(&workflow["step"]),
		);
	}
	let mut started: BTreeMap<Key, Option<u64>> = BTreeMap::new();
	let mut finished: BTreeMap<Key, Finished> = BTreeMap::new();
	let mut phase = "plan".to_owned();
	// The timestamp of the last event that is neither `completed` nor `warning`.
	let mut end = None;
	let mut timing = LoadTiming::default();
	for event in events {
		let event = event?;
		let name = text(&event, "event");
		if !matches!(name, "completed" | "warning") {
			end = Some(event["timestamp"].as_u64());
		}
		if event["phase"] == "measurement" && timing.start.is_none() {
			timing.start = Some(event["timestamp"].as_u64());
		}
		if event["phase"] == "teardown" {
			timing.stop = Some(event["timestamp"].as_u64());
		}
		match name {
			"phase" => {
				phase = if event["phase"] == "workflow" {
					format!("workflow:{}", text(&event, "id"))
				} else {
					text(&event, "phase").into()
				};
			},
			"step-started" | "step-finished" => {
				let key = key(&event);
				// Old runs may have no plan.json. Their event phases still identify suites.
				if !owners.contains_key(&key.0) {
					let index = suites.iter().position(|s| s.name == phase).unwrap_or_else(|| {
						suites.push(Suite { name: phase.clone(), cases: vec![] });
						suites.len() - 1
					});
					owners.insert(key.0.clone(), index);
				}
				if name == "step-started" {
					started.insert(key, event["timestamp"].as_u64());
				} else {
					finished.insert(
						key,
						Finished {
							success: event["success"] == true,
							elapsed_ms: event["elapsedMs"].as_u64().unwrap_or(0),
							error: event["error"].as_str().map(str::to_owned),
						},
					);
				}
			},
			_ => {},
		}
	}
	let end = end.flatten().unwrap_or(0);
	for (id, index) in owners {
		let mut cases = Vec::new();
		let (mut passed, mut passed_ms, mut failures, mut overflow_ms, mut invocations) =
			(0u64, 0u64, 0usize, 0u64, 0u64);
		for (key, event) in of_step(&finished, &id) {
			invocations += 1;
			if key.1.is_some() && event.success {
				passed += 1;
				passed_ms = passed_ms.saturating_add(event.elapsed_ms);
				continue;
			}
			if !event.success {
				failures += 1;
				if failures > FAILURES_PER_STEP {
					overflow_ms = overflow_ms.saturating_add(event.elapsed_ms);
					continue;
				}
			}
			cases.push(Case {
				name: case_name(key),
				ms: event.elapsed_ms,
				status: if event.success { Status::Passed } else { Status::Failed },
				message: event.error.clone().unwrap_or_else(|| "Step completed".into()),
			});
		}
		for (key, timestamp) in of_step(&started, &id) {
			if !finished.contains_key(key) {
				invocations += 1;
				failures += 1;
				if failures > FAILURES_PER_STEP {
					overflow_ms =
						overflow_ms.saturating_add(end.saturating_sub(timestamp.unwrap_or(end)));
					continue;
				}
				cases.push(Case {
					name: case_name(key),
					ms: end.saturating_sub(timestamp.unwrap_or(end)),
					status: Status::Failed,
					message: format!("Step interrupted: {}", text(outcome, "error")),
				});
			}
		}
		if passed > 0 {
			cases.push(Case {
				name: format!("{id} [passing invocations]"),
				ms: passed_ms,
				status: Status::Passed,
				message: format!("{passed} passing user/iteration invocations"),
			});
		}
		if failures > FAILURES_PER_STEP {
			cases.push(Case {
				name: format!("{id} [additional failures]"),
				ms: overflow_ms,
				status: Status::Failed,
				message: format!(
					"{} additional failed invocations; full details in events.jsonl",
					failures - FAILURES_PER_STEP
				),
			});
		}
		if let Some(workflow) = items(&plan["workflow"])
			.iter()
			.find(|w| items(&w["step"]).iter().any(|s| text(s, "@id") == id))
		{
			let expected = workflow["@users"]
				.as_u64()
				.unwrap_or(1)
				.saturating_mul(workflow["@iterations"].as_u64().unwrap_or(1));
			if invocations > 0 && invocations < expected {
				cases.push(Case {
					name: format!("{id} [not run]"),
					ms: 0,
					status: Status::Skipped,
					message: format!(
						"{} user/iteration invocations not run",
						expected - invocations
					),
				});
			}
		}
		if cases.is_empty() {
			cases.push(Case {
				name: id,
				ms: 0,
				status: Status::Skipped,
				message: "Step not run".into(),
			});
		}
		suites[index].cases.extend(cases);
	}
	let mut execution =
		mapped_case("execution".into(), text(outcome, "state"), text(outcome, "error"));
	// Step timings and load measurement timings are counted in their own suites.
	execution.ms = 0;
	suites[0].cases.push(execution);
	if !summary.is_null() || !plan["load"].is_null() {
		suites.push(load_suite(&timing, summary));
	}
	Ok(suites)
}

fn load_suite(timing: &LoadTiming, summary: &Value) -> Suite {
	let mut load = Suite { name: "load test".into(), cases: vec![] };
	let mut names = BTreeMap::new();
	for check in items(&summary["checks"]) {
		let mut name = format!("{}: {}", text(check, "outcome"), text(check, "check"));
		if let Some(source) = check["source"].as_str() {
			write!(name, " [evaluate:{source}]").unwrap();
		}
		let count = names.entry(name.clone()).or_insert(0);
		*count += 1;
		if *count > 1 {
			write!(name, " [check={count}]").unwrap();
		}
		load.cases.push(mapped_case(name, text(check, "status"), text(check, "detail")));
	}
	let mut verdict = mapped_case(
		"overall verdict".into(),
		text(&summary["verdict"], "result"),
		&format!("Load stop: {}", text(&summary["stop"], "detail")),
	);
	verdict.ms = timing.ms();
	load.cases.push(verdict);
	load
}

fn add_section(
	suites: &mut Vec<Suite>,
	owners: &mut BTreeMap<String, usize>,
	name: String,
	steps: &[Value],
) {
	if steps.is_empty() {
		return;
	}
	let index = suites.len();
	suites.push(Suite { name, cases: vec![] });
	for step in steps {
		owners.insert(text(step, "@id").into(), index);
	}
}

fn case_name((id, user, iteration): &Key) -> String {
	match (user, iteration) {
		(Some(user), Some(iteration)) => format!("{id} [user={user}, iteration={iteration}]"),
		_ => id.clone(),
	}
}

fn mapped_case(name: String, status: &str, detail: &str) -> Case {
	let state = match status {
		"fail" | "FAIL" | "failed" | "completed_with_failures" => Status::Failed,
		"pass" | "PASS" | "warn" | "WARN" | "info" | "completed" => Status::Passed,
		_ => Status::Skipped,
	};
	Case {
		name,
		ms: 0,
		status: state,
		message: if status.is_empty() {
			"No result recorded".into()
		} else {
			format!("{status}: {detail}")
		},
	}
}

// Terminal-safe, one line and bounded before any format-specific escaping.
fn single_line(text: &str) -> String {
	let mut out = String::new();
	for (i, c) in text.chars().enumerate() {
		if i == MESSAGE_CHARS {
			out.push('…');
			break;
		}
		match c {
			c if c.is_control() || c.is_whitespace() => out.push(' '),
			'\u{fffe}'
			| '\u{ffff}'
			| '\u{200e}'
			| '\u{200f}'
			| '\u{202a}'..='\u{202e}'
			| '\u{2066}'..='\u{2069}' => out.push(' '),
			c => out.push(c),
		}
	}
	out
}

// XML entities are added after bounding so they cannot be cut in half.
fn escape(text: &str) -> String {
	let mut out = String::new();
	for c in single_line(text).chars() {
		match c {
			'&' => out.push_str("&amp;"),
			'<' => out.push_str("&lt;"),
			'>' => out.push_str("&gt;"),
			'"' => out.push_str("&quot;"),
			'\'' => out.push_str("&apos;"),
			c => out.push(c),
		}
	}
	out
}

// Hash the complete identity before truncation; retain invocation/source suffixes too.
fn escape_name(name: &str, index: usize) -> String {
	if name.chars().count() <= MESSAGE_CHARS {
		return escape(name);
	}
	let hash = name
		.bytes()
		.fold(0xcbf29ce484222325u64, |h, b| (h ^ u64::from(b)).wrapping_mul(0x100000001b3));
	let tail: String = name.chars().rev().take(100).collect::<Vec<_>>().into_iter().rev().collect();
	escape(&format!(
		"{}…{tail} [id={hash:016x}-{index}]",
		name.chars().take(MESSAGE_CHARS - 150).collect::<String>()
	))
}

fn render_case(case: &Case, suite_name: &str, name: &str) -> String {
	let mut xml = format!(
		"    <testcase classname=\"{suite_name}\" name=\"{name}\" time=\"{:.3}\">\n",
		case.ms as f64 / 1000.0
	);
	let message = escape(&case.message);
	match case.status {
		Status::Failed => writeln!(xml, "      <failure message=\"{message}\">{message}</failure>"),
		Status::Skipped => writeln!(xml, "      <skipped>{message}</skipped>"),
		Status::Passed => writeln!(xml, "      <system-out>{message}</system-out>"),
	}
	.unwrap();
	xml.push_str("    </testcase>\n");
	xml
}

fn render(suites: &[Suite], directory: &Path) -> String {
	let mut xml = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<testsuites>\n");
	let mut omitted = [0u64; 3];
	let mut suite_names = BTreeMap::new();
	for (suite_index, suite) in suites.iter().enumerate() {
		let name = distinct_name(&suite.name, suite_index, &mut suite_names);
		let mut body = String::new();
		let mut counts = [0u64; 3];
		let mut ms = 0u64;
		let mut names = BTreeMap::new();
		for (index, case) in suite.cases.iter().enumerate() {
			let case_name = distinct_name(&case.name, index, &mut names);
			let rendered = render_case(case, &name, &case_name);
			let status = match case.status {
				Status::Passed => 0,
				Status::Failed => 1,
				Status::Skipped => 2,
			};
			// Reserve ample space for suite metadata, closing tags and the overflow summary.
			if xml.len() + body.len() + rendered.len() + SUITE_RESERVE > REPORT_BYTES {
				omitted[status] += 1;
				continue;
			}
			body.push_str(&rendered);
			counts[status] += 1;
			ms = ms.saturating_add(case.ms);
		}
		if counts.iter().sum::<u64>() == 0 {
			continue;
		}
		writeln!(xml, "  <testsuite name=\"{name}\" tests=\"{}\" failures=\"{}\" errors=\"0\" skipped=\"{}\" time=\"{:.3}\">", counts.iter().sum::<u64>(), counts[1], counts[2], ms as f64 / 1000.0).unwrap();
		writeln!(
			xml,
			"    <properties><property name=\"run-directory\" value=\"{}\"/></properties>",
			escape(&directory.display().to_string())
		)
		.unwrap();
		xml.push_str(&body);
		writeln!(
			xml,
			"    <system-out>Run directory: {}</system-out>",
			escape(&directory.display().to_string())
		)
		.unwrap();
		xml.push_str("  </testsuite>\n");
	}
	if omitted.iter().sum::<u64>() > 0 {
		xml.push_str("  <testsuite name=\"report overflow\" tests=\"1\" failures=\"");
		xml.push_str(if omitted[1] > 0 { "1" } else { "0" });
		xml.push_str("\" errors=\"0\" skipped=\"0\" time=\"0.000\">\n");
		xml.push_str(&render_case(&Case { name: "additional results".into(), ms: 0,
			status: if omitted[1] > 0 { Status::Failed } else { Status::Passed },
			message: format!("Report size limit: {} passed, {} failed, {} skipped cases omitted (cases may aggregate invocations); full details in events.jsonl and summary.json", omitted[0], omitted[1], omitted[2]) }, "report overflow", "additional results"));
		xml.push_str("  </testsuite>\n");
	}
	xml.push_str("</testsuites>\n");
	xml
}

fn distinct_name(name: &str, index: usize, used: &mut BTreeMap<String, usize>) -> String {
	let mut safe = escape_name(name, index);
	// XML sanitization can merge different names; reserve every emitted name.
	while used.insert(safe.clone(), index).is_some() {
		safe = format!("{} [case={index}]", safe);
	}
	safe
}

#[cfg(test)]
mod tests {
	use super::*;
	use serde_json::json;

	fn ok(events: Vec<Value>) -> impl Iterator<Item = Result<Value>> {
		events.into_iter().map(Ok)
	}

	#[test]
	fn a_bad_event_line_is_an_error_and_writes_no_report() {
		let dir = std::env::temp_dir().join(format!("polkameter-junit-bad-{}", std::process::id()));
		fs::create_dir_all(&dir).unwrap();
		fs::write(dir.join("execution.json"), r#"{"state":"completed"}"#).unwrap();
		fs::write(
			dir.join("events.jsonl"),
			"{\"event\":\"phase\",\"phase\":\"setup\"}\nnot json\n",
		)
		.unwrap();
		assert!(write(&dir, &[]).is_err());
		assert!(!dir.join("junit.xml").exists());
		fs::remove_dir_all(&dir).unwrap();
	}

	#[test]
	fn measurement_time_spans_the_first_measurement_to_the_last_teardown() {
		let events = vec![
			json!({"event":"phase","phase":"measurement","timestamp":100}),
			json!({"event":"phase","phase":"measurement","timestamp":150}),
			json!({"event":"phase","phase":"teardown","timestamp":300}),
			json!({"event":"phase","phase":"teardown","timestamp":400}),
		];
		let summary = json!({"verdict":{"result":"PASS"},"stop":{"detail":"rate cap"}});
		let suites = suites(&Value::Null, ok(events), &summary, &json!({})).unwrap();
		let verdict = suites.last().unwrap().cases.last().unwrap();
		assert_eq!(verdict.name, "overall verdict");
		assert_eq!(verdict.ms, 300);
	}

	#[test]
	fn maps_every_check_and_overall_status() {
		for (status, want) in [
			("pass", Status::Passed),
			("fail", Status::Failed),
			("warn", Status::Passed),
			("info", Status::Passed),
			("no result", Status::Skipped),
			("PASS", Status::Passed),
			("FAIL", Status::Failed),
			("WARN", Status::Passed),
			("INCONCLUSIVE", Status::Skipped),
			("", Status::Skipped),
		] {
			let case = mapped_case("check".into(), status, "useful detail");
			assert_eq!(case.status, want, "{status}");
			let xml =
				render(&[Suite { name: "load test".into(), cases: vec![case] }], Path::new("/run"));
			assert!(xml.contains(match want {
				Status::Failed => "<failure message=",
				Status::Skipped => "<skipped>",
				Status::Passed => "<system-out>",
			}));
			if status == "warn" {
				assert!(xml.contains("<system-out>warn: useful detail</system-out>"));
			}
		}
	}

	#[test]
	fn escapes_xml_and_removes_controls_without_cutting_entities_or_unicode() {
		assert_eq!(escape("<&>\"'"), "&lt;&amp;&gt;&quot;&apos;");
		assert_eq!(escape("a\n\r\t\0\u{1b}\u{85}\u{202e}\u{fffe}\u{ffff}b"), "a         b");
		assert_eq!(
			escape(&format!("{}&", "🦀".repeat(2047))),
			format!("{}&amp;", "🦀".repeat(2047))
		);
		assert_eq!(escape(&"界".repeat(10000)), format!("{}…", "界".repeat(2048)));
	}

	#[test]
	fn maps_step_failures_unstarted_and_interrupted_steps() {
		let plan = json!({"setup":{"step":[{"@id":"ok"},{"@id":"bad"},{"@id":"unstarted"},{"@id":"interrupted"}]}});
		let events = vec![
			json!({"event":"step-finished","step":"ok","success":true,"elapsedMs":123}),
			json!({"event":"step-finished","step":"bad","success":false,"error":"assertion failed"}),
			json!({"event":"step-started","step":"interrupted","timestamp":100}),
			json!({"event":"phase","phase":"teardown","timestamp":250}),
			// Copy/report warnings after completion must not extend interrupted step timings.
			json!({"event":"warning","timestamp":999999}),
		];
		let suites =
			suites(&plan, ok(events), &Value::Null, &json!({"state":"failed","error":"deadline"}))
				.unwrap();
		let cases = &suites[1].cases;
		let find = |name| cases.iter().find(|c| c.name == name).unwrap();
		assert_eq!(find("ok").status, Status::Passed);
		assert_eq!(find("ok").ms, 123);
		assert_eq!(find("bad").status, Status::Failed);
		assert_eq!(find("unstarted").status, Status::Skipped);
		assert_eq!(find("interrupted").status, Status::Failed);
		assert_eq!(find("interrupted").ms, 150);
		let xml = render(&suites, Path::new("/runs/a&b"));
		assert!(
			xml.contains("tests=\"4\" failures=\"2\" errors=\"0\" skipped=\"1\" time=\"0.273\"")
		);
		assert!(xml.contains("Run directory: /runs/a&amp;b"));
	}

	#[test]
	fn workflow_invocations_are_unique_and_missing_work_is_bounded() {
		let plan = json!({"workflow":[{"@id":"w","@users":100000,"@iterations":4000000000u64,
			"step":[{"@id":"a"},{"@id":"b"}]}]});
		let events = vec![
			json!({"event":"step-finished","step":"a","success":true,"user":0,"iteration":0}),
			json!({"event":"step-finished","step":"a","success":false,"user":1,"iteration":0}),
		];
		let suites = suites(&plan, ok(events), &Value::Null, &json!({"state":"failed"})).unwrap();
		let cases = &suites[1].cases;
		assert_eq!(cases.len(), 4);
		assert_eq!(cases[0].name, "a [user=1, iteration=0]");
		assert_eq!(cases[1].name, "a [passing invocations]");
		assert_eq!(cases[2].status, Status::Skipped);
		assert_eq!(cases[2].message, "399999999999998 user/iteration invocations not run");
	}

	#[test]
	fn completed_large_workflow_aggregates_passes_and_retains_failures() {
		let plan = json!({"workflow":[{"@id":"workers","@users":1000,"@iterations":200,"step":[{"@id":"work"}]}]});
		let events: Vec<_> = (0..1000).flat_map(|user| (0..200).map(move |iteration| {
			json!({"event":"step-finished", "step":"work", "user":user,"iteration":iteration,
				"success": !(user == 0 && iteration < 150), "elapsedMs":1,
				"error":if user == 0 && iteration < 150 { json!(format!("failure {iteration}")) } else { Value::Null }})
		})).collect();
		let suites =
			suites(&plan, ok(events), &Value::Null, &json!({"state":"completed"})).unwrap();
		let cases = &suites[1].cases;
		assert_eq!(cases.len(), FAILURES_PER_STEP + 2);
		assert_eq!(cases.iter().filter(|c| c.status == Status::Failed).count(), 101);
		for i in 0..100 {
			assert!(cases.iter().any(|c| c.message == format!("failure {i}")));
		}
		assert!(
			cases
				.iter()
				.any(|c| c.message
					== "50 additional failed invocations; full details in events.jsonl")
		);
		assert!(cases.iter().any(|c| c.message == "199850 passing user/iteration invocations"));
		assert_eq!(cases.iter().map(|c| c.ms).sum::<u64>(), 200000);
		let xml = render(&suites, Path::new("/runs/large"));
		eprintln!("large synthetic report: {} bytes", xml.len());
		assert!(xml.len() < 5 * 1024 * 1024);
		assert!(xml.len() < 30000);
	}

	#[test]
	fn whole_report_budget_keeps_failure_summary_even_with_many_steps() {
		let suite = Suite {
			name: "huge".into(),
			cases: (0..10000)
				.map(|i| Case {
					name: format!("step{i}"),
					ms: 1,
					status: Status::Failed,
					message: "&".repeat(MESSAGE_CHARS),
				})
				.collect(),
		};
		let xml = render(&[suite], Path::new("/runs/large"));
		assert!(xml.len() <= REPORT_BYTES);
		assert!(xml.contains("report overflow"));
		assert!(xml.contains("full details in events.jsonl and summary.json"));
		let emitted = xml.matches("<failure message=").count() - 1;
		assert!(xml.contains(&format!("{} failed", 10000 - emitted)));
	}

	#[test]
	fn checks_keep_source_and_duplicate_counter_and_long_names_keep_identity() {
		let summary = json!({"checks":[
			{"outcome":"pool","check":"same","status":"pass","source":"first"},
			{"outcome":"pool","check":"same","status":"fail","source":"second"},
			{"outcome":"pool","check":"same","status":"fail","source":"second"}
		]});
		let suite = load_suite(&LoadTiming::default(), &summary);
		assert_eq!(suite.cases[0].name, "pool: same [evaluate:first]");
		assert_eq!(suite.cases[1].name, "pool: same [evaluate:second]");
		assert_eq!(suite.cases[2].name, "pool: same [evaluate:second] [check=2]");
		let long = "界".repeat(MESSAGE_CHARS * 2);
		let a = escape_name(&case_name(&(format!("{long}a"), Some(1), Some(2))), 0);
		let b = escape_name(&case_name(&(format!("{long}b"), Some(1), Some(2))), 1);
		assert_ne!(a, b);
		assert!(a.contains("a [user=1, iteration=2]"));
		assert!(b.contains("b [user=1, iteration=2]"));
		assert!(a.contains("[id="));
		let mut used = BTreeMap::new();
		assert_ne!(distinct_name("a\nb", 0, &mut used), distinct_name("a b", 1, &mut used));
	}

	#[test]
	fn aborted_load_has_a_skipped_verdict_and_failed_execution() {
		let suites = suites(
			&json!({"load":{}}),
			ok(vec![]),
			&Value::Null,
			&json!({"state":"failed","error":"node down"}),
		)
		.unwrap();
		assert_eq!(suites[0].cases[0].status, Status::Failed);
		assert_eq!(suites[1].cases[0].status, Status::Skipped);
		assert_eq!(suites[1].cases[0].message, "No result recorded");
	}
}
