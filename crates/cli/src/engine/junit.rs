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

use crate::redact::Redactor;

/// Maximum message/name length before XML escaping (entities require at most six bytes).
pub const MESSAGE_CHARS: usize = 2048;
const FAILURES_PER_STEP: usize = 100;
const REPORT_BYTES: usize = 4 * 1024 * 1024;
const XML_CHAR_BYTES: usize = 6;
const SUITE_RESERVE: usize = MESSAGE_CHARS * XML_CHAR_BYTES * 6 + 4096;

/// Best-effort JUnit warning. Never changes an execution outcome, even if event recording fails.
/// Remote clients emit the returned event in their own event stream (no local run directory).
pub fn warning(directory: Option<&Path>, message: &str, secrets: &[String]) -> Value {
	let message = Redactor::new(secrets, None::<&str>).text(&format!("JUnit report: {message}"));
	eprintln!("{} warning: {}", crate::PRODUCT_NAME, single_line(&message));
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

/// Writes `junit.xml`. Run-time callers pass the run's redactor, which redacted its events;
/// offline callers one of the credentials available now, over the already-redacted files. Only recorded
/// error summaries (including bounded assertion values), not raw inputs/outputs, are exported.
///
/// `events.jsonl` is read one line at a time and only what the report needs from each step
/// event is kept, so a long run's event log is never held in memory.
pub fn write(directory: &Path, redactor: &Redactor) -> Result<()> {
	let mut plan = read_optional(&directory.join("plan.json"))?;
	let mut summary = read_optional(&directory.join("summary.json"))?;
	let mut outcome: Value = serde_json::from_slice(&fs::read(directory.join("execution.json"))?)?;
	for value in [&mut plan, &mut summary, &mut outcome] {
		redactor.value(value);
	}
	let file = fs::File::open(directory.join("events.jsonl"))?;
	let events = BufReader::new(file).lines().map(|line| -> Result<Value> {
		let mut event = serde_json::from_str(&line?)?;
		redactor.value(&mut event);
		Ok(event)
	});
	let suites = suites(&plan, events, &summary, &outcome)?;
	let safe_path = redactor.text(&directory.display().to_string());

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

/// One step's finished invocations, aggregated as they are read: passing user/iteration
/// invocations only add to a count and a duration, and at most [`FAILURES_PER_STEP`] failures
/// are kept, the first in key order; the rest only add their duration. Memory stays bounded
/// however many invocations a run has.
#[derive(Default)]
struct StepRecord {
	/// Finished invocations.
	finished: u64,
	/// Passing user/iteration invocations.
	passed: u64,
	/// Their total time.
	passed_ms: u64,
	/// Failed invocations.
	failures: usize,
	/// The total time of the failures not kept.
	overflow_ms: u64,
	/// Kept cases in key order: the first failures, and passes outside a workflow.
	kept: BTreeMap<Key, Finished>,
	/// How many of `kept` are failures.
	kept_failures: usize,
}

impl StepRecord {
	fn finish(&mut self, key: Key, event: Finished) {
		self.finished += 1;
		if key.1.is_some() && event.success {
			self.passed += 1;
			self.passed_ms = self.passed_ms.saturating_add(event.elapsed_ms);
			return;
		}
		let failed = !event.success;
		if let Some(old) = self.kept.insert(key, event) {
			// The same invocation finished twice: the later record replaces the earlier.
			self.finished -= 1;
			if !old.success {
				self.failures -= 1;
				self.kept_failures -= 1;
			}
		}
		if !failed {
			return;
		}
		self.failures += 1;
		self.kept_failures += 1;
		if self.kept_failures > FAILURES_PER_STEP {
			let last = self.kept.iter().rev().find(|(_, f)| !f.success).map(|(k, _)| k.clone());
			if let Some(f) = last.and_then(|k| self.kept.remove(&k)) {
				self.overflow_ms = self.overflow_ms.saturating_add(f.elapsed_ms);
				self.kept_failures -= 1;
			}
		}
	}
}

/// Builds the suites from the plan and the recorded events, read once in order. Only bounded
/// aggregates per step are kept, never the events themselves.
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
	// Started and not finished: what is left at the end was interrupted.
	let mut started: BTreeMap<Key, Option<u64>> = BTreeMap::new();
	let mut records: BTreeMap<String, StepRecord> = BTreeMap::new();
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
					started.remove(&key);
					records.entry(key.0.clone()).or_default().finish(
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
		let record = records.remove(&id).unwrap_or_default();
		let (passed, passed_ms) = (record.passed, record.passed_ms);
		let (mut failures, mut overflow_ms, mut invocations) =
			(record.failures, record.overflow_ms, record.finished);
		for (key, event) in &record.kept {
			cases.push(Case {
				name: case_name(key),
				ms: event.elapsed_ms,
				status: if event.success { Status::Passed } else { Status::Failed },
				message: event.error.clone().unwrap_or_else(|| "Step completed".into()),
			});
		}
		let interrupted = started.range((id.clone(), None, None)..).take_while(|(k, _)| k.0 == id);
		for (key, timestamp) in interrupted {
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
	let mut execution = mapped_case(
		"execution".into(),
		text(outcome, "state"),
		&execution_detail(summary, outcome),
	);
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
	let result = text(&summary["verdict"], "result");
	let mut detail = format!("Load stop: {}", text(&summary["stop"], "detail"));
	if result == "INCONCLUSIVE" {
		// Say what is missing: a run that fails on INCONCLUSIVE must explain itself here.
		if let Some(reason) = inconclusive_reason(summary) {
			write!(detail, "; {reason}").unwrap();
		}
	}
	let mut verdict = mapped_case("overall verdict".into(), result, &detail);
	verdict.ms = timing.ms();
	load.cases.push(verdict);
	load
}

/// Why an INCONCLUSIVE verdict is inconclusive, by the rules that set it; `None` when the summary
/// cannot be read as one.
fn inconclusive_reason(summary: &Value) -> Option<String> {
	use polkameter_plugin_sdk::observers::{checks::CheckResult, files::summary::Summary};
	let checks: Vec<CheckResult> = serde_json::from_value(summary["checks"].clone()).ok()?;
	let summary: Summary = serde_json::from_value(summary.clone()).ok()?;
	Some(crate::engine::measurement::verdict_reason(&summary, &checks))
}

/// The execution case's detail: the run's error, or for a run that completed with failures and
/// no error, why it exits 1: its recorded `exit_reason` (smoke problems, or the verdict and the
/// flag), else, for a recording from before 0.3.0, its exit code and load verdict.
fn execution_detail(summary: &Value, outcome: &Value) -> String {
	let error = text(outcome, "error");
	if !error.is_empty() || text(outcome, "state") != "completed_with_failures" {
		return error.into();
	}
	let reason = text(outcome, "exit_reason");
	if !reason.is_empty() {
		return reason.into();
	}
	let verdict = text(&summary["verdict"], "result");
	if verdict.is_empty() {
		return String::new();
	}
	let code = outcome["exit_code"].as_i64().unwrap_or(1);
	format!("exit code {code} with load verdict {verdict}")
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
			c if c.is_whitespace() || crate::redact::is_unsafe_char(c) => out.push(' '),
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
	let mut safe = escape(name);
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

	/// FAIL is a failure, INCONCLUSIVE or no result is skipped, and a failed execution is never
	/// green.
	#[test]
	fn statuses_map_to_failures_and_skips() {
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
		}
		// An INCONCLUSIVE run that exited 1: the verdict is skipped, the execution failed.
		let mut summary: Value =
			serde_json::from_str(include_str!("../../tests/fixtures/smoke-run/summary.json"))
				.unwrap();
		summary["breakingPoint"] = Value::Null;
		summary["verdict"] = json!({"result": "INCONCLUSIVE"});
		summary["checks"] = json!([{"outcome":"run","check":"plugin check",
			"status":"no result","detail":"no data","optional":false}]);
		let outcome = json!({"state":"completed_with_failures","exit_code":1,"error":null});
		let inconclusive = suites(&Value::Null, ok(vec![]), &summary, &outcome).unwrap();
		assert_eq!(inconclusive[0].cases[0].status, Status::Failed);
		assert_eq!(inconclusive[1].cases.last().unwrap().status, Status::Skipped);
		assert!(render(&inconclusive, Path::new("/run")).contains("failures=\"1\""));
		// A load that aborted before a summary: no result is skipped, the execution failed.
		let outcome = json!({"state":"failed","error":"node down"});
		let suites = suites(&json!({"load":{}}), ok(vec![]), &Value::Null, &outcome).unwrap();
		assert_eq!(suites[0].cases[0].status, Status::Failed);
		assert_eq!(suites[1].cases[0].status, Status::Skipped);
	}

	#[test]
	fn report_size_stays_bounded() {
		// Steps never run in a huge workflow are one skipped case, not one per invocation.
		let plan = json!({"workflow":[{"@id":"w","@users":100000,"@iterations":4000000000u64,
			"step":[{"@id":"a"},{"@id":"b"}]}]});
		let events = vec![
			json!({"event":"step-finished","step":"a","success":true,"user":0,"iteration":0}),
			json!({"event":"step-finished","step":"a","success":false,"user":1,"iteration":0}),
		];
		let unrun = suites(&plan, ok(events), &Value::Null, &json!({"state":"failed"})).unwrap();
		assert_eq!(unrun[1].cases.len(), 4);
		assert_eq!(unrun[1].cases[2].status, Status::Skipped);
		// 200,000 invocations: the first failures are kept, the rest and the passes aggregated.
		let plan = json!({"workflow":[{"@id":"workers","@users":1000,"@iterations":200,"step":[{"@id":"work"}]}]});
		let events: Vec<_> = (0..1000)
			.flat_map(|user| {
				(0..200).map(move |iteration| {
					json!({"event":"step-finished", "step":"work", "user":user,"iteration":iteration,
				"success": !(user == 0 && iteration < 150), "elapsedMs":1})
				})
			})
			.collect();
		let suites =
			suites(&plan, ok(events), &Value::Null, &json!({"state":"completed"})).unwrap();
		let cases = &suites[1].cases;
		assert_eq!(cases.len(), FAILURES_PER_STEP + 2);
		assert_eq!(cases.iter().filter(|c| c.status == Status::Failed).count(), 101);
		assert_eq!(cases.iter().map(|c| c.ms).sum::<u64>(), 200000);
		assert!(render(&suites, Path::new("/runs/large")).len() < 30000);
		// 10,000 failed steps with the longest messages still fit the whole-report budget.
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
	}
}
