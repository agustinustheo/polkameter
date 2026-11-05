//! JUnit reports derived only from recorded events, plan structure and load results.
use std::{collections::BTreeMap, fmt::Write as _, fs, path::Path};

use anyhow::Result;
use serde_json::Value;

use crate::execute::redact_text;

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
/// callers use the already-redacted recorded files. No step inputs or outputs are exported.
pub fn write(directory: &Path, secrets: &[String]) -> Result<()> {
	let events: Vec<Value> = fs::read_to_string(directory.join("events.jsonl"))?
		.lines()
		.map(serde_json::from_str)
		.collect::<std::result::Result<_, _>>()?;
	let plan = read_optional(&directory.join("plan.json"))?;
	let summary = read_optional(&directory.join("summary.json"))?;
	let outcome: Value = serde_json::from_slice(&fs::read(directory.join("execution.json"))?)?;
	let suites = suites(&plan, &events, &summary, &outcome);
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

fn suites(plan: &Value, events: &[Value], summary: &Value, outcome: &Value) -> Vec<Suite> {
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
	let mut started = BTreeMap::new();
	let mut finished = BTreeMap::new();
	let mut phase = "plan".to_owned();
	for event in events {
		match text(event, "event") {
			"phase" => {
				phase = if event["phase"] == "workflow" {
					format!("workflow:{}", text(event, "id"))
				} else {
					text(event, "phase").into()
				};
			},
			"step-started" | "step-finished" => {
				let key = key(event);
				// Old runs may have no plan.json. Their event phases still identify suites.
				if !owners.contains_key(&key.0) {
					let index = suites.iter().position(|s| s.name == phase).unwrap_or_else(|| {
						suites.push(Suite { name: phase.clone(), cases: vec![] });
						suites.len() - 1
					});
					owners.insert(key.0.clone(), index);
				}
				if event["event"] == "step-started" {
					started.insert(key, event);
				} else {
					finished.insert(key, event);
				}
			},
			_ => {},
		}
	}
	let end = events
		.iter()
		.rev()
		.find(|e| e["event"] != "completed")
		.and_then(|e| e["timestamp"].as_u64())
		.unwrap_or(0);
	for (id, index) in owners {
		let mut cases = Vec::new();
		for (key, event) in finished.iter().filter(|(key, _)| key.0 == id) {
			cases.push(Case {
				name: case_name(key),
				ms: event["elapsedMs"].as_u64().unwrap_or(0),
				status: if event["success"] == true { Status::Passed } else { Status::Failed },
				message: event["error"].as_str().unwrap_or("Step completed").into(),
			});
		}
		for (key, event) in started.iter().filter(|(key, _)| key.0 == id) {
			if !finished.contains_key(key) {
				cases.push(Case {
					name: case_name(key),
					ms: end.saturating_sub(event["timestamp"].as_u64().unwrap_or(end)),
					status: Status::Failed,
					message: format!("Step interrupted: {}", text(outcome, "error")),
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
	add_missing_workflow_cases(&mut suites, plan);
	let mut execution =
		mapped_case("execution".into(), text(outcome, "state"), text(outcome, "error"));
	// Step timings and load measurement timings are counted in their own suites.
	execution.ms = 0;
	suites[0].cases.push(execution);
	if !summary.is_null() || !plan["load"].is_null() {
		suites.push(load_suite(events, summary));
	}
	suites
}

fn load_suite(events: &[Value], summary: &Value) -> Suite {
	let mut load = Suite { name: "load test".into(), cases: vec![] };
	for check in items(&summary["checks"]) {
		load.cases.push(mapped_case(
			format!("{}: {}", text(check, "outcome"), text(check, "check")),
			text(check, "status"),
			text(check, "detail"),
		));
	}
	let mut verdict = mapped_case(
		"overall verdict".into(),
		text(&summary["verdict"], "result"),
		&format!("Load stop: {}", text(&summary["stop"], "detail")),
	);
	let start = events.iter().find(|e| e["phase"] == "measurement");
	let stop = events.iter().rev().find(|e| e["phase"] == "teardown");
	verdict.ms = start.zip(stop).map_or(0, |(start, stop)| {
		stop["timestamp"]
			.as_u64()
			.unwrap_or(0)
			.saturating_sub(start["timestamp"].as_u64().unwrap_or(0))
	});
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

// A partially executed workflow can have billions of unstarted invocations. Keep one skipped
// case per step for the missing invocations, rather than expanding an unbounded Cartesian product.
fn add_missing_workflow_cases(suites: &mut [Suite], plan: &Value) {
	for workflow in items(&plan["workflow"]) {
		let name = format!("workflow:{}", text(workflow, "@id"));
		let Some(suite) = suites.iter_mut().find(|s| s.name == name) else { continue };
		let expected = workflow["@users"]
			.as_u64()
			.unwrap_or(1)
			.saturating_mul(workflow["@iterations"].as_u64().unwrap_or(1));
		for step in items(&workflow["step"]) {
			let id = text(step, "@id");
			let prefix = format!("{id} [user=");
			let count = suite.cases.iter().filter(|c| c.name.starts_with(&prefix)).count() as u64;
			if count > 0 && count < expected {
				suite.cases.push(Case {
					name: format!("{id} [not run]"),
					ms: 0,
					status: Status::Skipped,
					message: format!("{} user/iteration invocations not run", expected - count),
				});
			}
		}
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

// XML 1.0, terminal-safe, one line, bounded before escaping so entities cannot be cut in half.
fn escape(text: &str) -> String {
	let mut out = String::new();
	for (i, c) in text.chars().enumerate() {
		if i == 2048 {
			out.push('…');
			break;
		}
		match c {
			'&' => out.push_str("&amp;"),
			'<' => out.push_str("&lt;"),
			'>' => out.push_str("&gt;"),
			'"' => out.push_str("&quot;"),
			'\'' => out.push_str("&apos;"),
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

fn render(suites: &[Suite], directory: &Path) -> String {
	let mut xml = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<testsuites>\n");
	for suite in suites {
		let count = |status| suite.cases.iter().filter(|c| c.status == status).count();
		let ms: u64 = suite.cases.iter().map(|c| c.ms).sum();
		let name = escape(&suite.name);
		writeln!(xml, "  <testsuite name=\"{name}\" tests=\"{}\" failures=\"{}\" errors=\"0\" skipped=\"{}\" time=\"{:.3}\">",
			suite.cases.len(), count(Status::Failed), count(Status::Skipped), ms as f64 / 1000.0).unwrap();
		writeln!(
			xml,
			"    <properties><property name=\"run-directory\" value=\"{}\"/></properties>",
			escape(&directory.display().to_string())
		)
		.unwrap();
		for case in &suite.cases {
			writeln!(
				xml,
				"    <testcase classname=\"{name}\" name=\"{}\" time=\"{:.3}\">",
				escape(&case.name),
				case.ms as f64 / 1000.0
			)
			.unwrap();
			let message = escape(&case.message);
			match case.status {
				Status::Failed => {
					writeln!(xml, "      <failure message=\"{message}\">{message}</failure>")
				},
				Status::Skipped => writeln!(xml, "      <skipped>{message}</skipped>"),
				Status::Passed => writeln!(xml, "      <system-out>{message}</system-out>"),
			}
			.unwrap();
			xml.push_str("    </testcase>\n");
		}
		writeln!(
			xml,
			"    <system-out>Run directory: {}</system-out>",
			escape(&directory.display().to_string())
		)
		.unwrap();
		xml.push_str("  </testsuite>\n");
	}
	xml.push_str("</testsuites>\n");
	xml
}

#[cfg(test)]
mod tests {
	use super::*;
	use serde_json::json;

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
		];
		let suites =
			suites(&plan, &events, &Value::Null, &json!({"state":"failed","error":"deadline"}));
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
		let suites = suites(&plan, &events, &Value::Null, &json!({"state":"failed"}));
		let cases = &suites[1].cases;
		assert_eq!(cases.len(), 4);
		assert_eq!(cases[0].name, "a [user=0, iteration=0]");
		assert_eq!(cases[1].name, "a [user=1, iteration=0]");
		assert_eq!(cases[2].status, Status::Skipped);
		assert_eq!(cases[3].message, "399999999999998 user/iteration invocations not run");
	}

	#[test]
	fn aborted_load_has_a_skipped_verdict_and_failed_execution() {
		let suites = suites(
			&json!({"load":{}}),
			&[],
			&Value::Null,
			&json!({"state":"failed","error":"node down"}),
		);
		assert_eq!(suites[0].cases[0].status, Status::Failed);
		assert_eq!(suites[1].cases[0].status, Status::Skipped);
		assert_eq!(suites[1].cases[0].message, "No result recorded");
	}
}
